use std::{sync::Arc, time::Instant};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use tokio::sync::OwnedSemaphorePermit;
use tracing::{error, warn};

use crate::server::{
    AppState,
    response::{error_response, text_response},
};
use crate::{
    config::Config,
    iiif::{self, Route},
    jp2, jxl,
    metrics::Metrics,
    pipeline,
    storage::{Derivative, SourceNotFound, hash},
};

struct RenderedBytes {
    data: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

impl AsRef<[u8]> for RenderedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

fn encoded_identifier(identifier: &str) -> String {
    let mut out = String::new();
    for byte in identifier.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn base_uri(config: &Config, identifier: &str) -> String {
    format!(
        "{}{}/{}",
        config.public_base_url,
        config.iiif_prefix,
        encoded_identifier(identifier)
    )
}

fn iiif_path<'a>(raw_path: &'a str, route_prefix: &str) -> Option<&'a str> {
    raw_path
        .strip_prefix(route_prefix)
        .filter(|path| path.starts_with('/'))
}

pub(crate) fn requested_identifier(raw_path: &str, route_prefix: &str) -> Option<String> {
    let path = iiif_path(raw_path, route_prefix)?;
    match iiif::parse_route(path).ok()? {
        Route::Base(id) | Route::Info(id) => Some(id),
        Route::Image(request) => Some(request.identifier),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InfoMedia {
    JsonLd,
    Json,
}

fn info_media(headers: &HeaderMap) -> Option<InfoMedia> {
    if !headers.contains_key(header::ACCEPT) {
        return Some(InfoMedia::JsonLd);
    }
    let mut json_ld: Option<(u8, f64)> = None;
    let mut json: Option<(u8, f64)> = None;
    for value in headers.get_all(header::ACCEPT) {
        let Ok(value) = value.to_str() else {
            continue;
        };
        for item in value.split(',') {
            let mut parts = item.split(';');
            let media_type = parts.next().unwrap_or("").trim().to_ascii_lowercase();
            let mut quality = 1.0;
            let mut profile_valid = true;
            for parameter in parts {
                if let Some((name, value)) = parameter.trim().split_once('=') {
                    if name.trim().eq_ignore_ascii_case("q") {
                        quality = value.trim().parse::<f64>().unwrap_or(0.0);
                    } else if name.trim().eq_ignore_ascii_case("profile") {
                        profile_valid = value.trim().trim_matches('"')
                            == "http://iiif.io/api/image/3/context.json";
                    }
                }
            }
            if !quality.is_finite() || !(0.0..=1.0).contains(&quality) {
                continue;
            }
            let candidates: &[(InfoMedia, u8)] = match media_type.as_str() {
                "application/ld+json" if profile_valid => &[(InfoMedia::JsonLd, 2)],
                "application/json" => &[(InfoMedia::Json, 2)],
                "application/*" => &[(InfoMedia::JsonLd, 1), (InfoMedia::Json, 1)],
                "*/*" => &[(InfoMedia::JsonLd, 0), (InfoMedia::Json, 0)],
                _ => &[],
            };
            for &(media, specificity) in candidates {
                let current = match media {
                    InfoMedia::JsonLd => &mut json_ld,
                    InfoMedia::Json => &mut json,
                };
                if current
                    .as_ref()
                    .is_none_or(|(spec, q)| (specificity, quality) > (*spec, *q))
                {
                    *current = Some((specificity, quality));
                }
            }
        }
    }
    let ld_quality = json_ld.map(|(_, q)| q).unwrap_or(0.0);
    let json_quality = json.map(|(_, q)| q).unwrap_or(0.0);
    if ld_quality == 0.0 && json_quality == 0.0 {
        None
    } else if json_quality > ld_quality {
        Some(InfoMedia::Json)
    } else {
        Some(InfoMedia::JsonLd)
    }
}

fn info_document(config: &Config, identifier: &str, width: u32, height: u32) -> serde_json::Value {
    let mut sizes = Vec::new();
    let (mut w, mut h) = iiif::max_size(width, height, config.max_output_pixels);
    while w >= config.min_size && h >= config.min_size {
        sizes.push(serde_json::json!({"width": w, "height": h}));
        if w <= 1 || h <= 1 {
            break;
        }
        w = w.div_ceil(2);
        h = h.div_ceil(2);
    }
    sizes.reverse();
    let tile_width = config.min_tile_size.max(1);
    let mut factors = Vec::new();
    let mut factor = 1u32;
    loop {
        factors.push(factor);
        if width.max(height) / factor <= tile_width || factor > u32::MAX / 2 {
            break;
        }
        factor *= 2;
    }
    let mut info = serde_json::json!({
        "@context": "http://iiif.io/api/image/3/context.json",
        "id": base_uri(config, identifier),
        "type": "ImageService3",
        "protocol": "http://iiif.io/api/image",
        "profile": "level2",
        "width": width,
        "height": height,
        "maxArea": config.max_output_pixels,
        "preferredFormats": ["webp", "avif", "jpg"],
        "extraFormats": ["webp", "avif", "jxl"],
        "extraQualities": ["color", "gray"]
    });
    if !sizes.is_empty() {
        info["sizes"] = serde_json::json!(sizes);
    }
    if width.max(height) >= tile_width
        && u64::from(tile_width) * u64::from(tile_width) <= config.max_output_pixels
    {
        info["tiles"] = serde_json::json!([{"width": tile_width, "scaleFactors": factors}]);
    }
    info
}

pub(crate) async fn iiif_handler(
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    }
    let counters = state.storage.metrics.clone();
    let mut response = iiif_handler_inner(state, uri, &headers).await;
    if response.status().is_server_error() {
        Metrics::increment(&counters.errors);
    }
    if method == Method::HEAD {
        *response.body_mut() = Body::empty();
    }
    response
}

async fn iiif_handler_inner(state: Arc<AppState>, uri: Uri, headers: &HeaderMap) -> Response {
    let raw_path = uri.path();
    let path = match iiif_path(raw_path, &state.config.route_prefix) {
        Some(path) => path,
        None => return text_response(StatusCode::NOT_FOUND, "Not found"),
    };
    let route = match iiif::parse_route(path) {
        Ok(route) => route,
        Err(e) if e.0 == "invalid IIIF route" => return error_response(StatusCode::NOT_FOUND, e),
        Err(e) if e.0 == "upscaling is not supported" => {
            return error_response(StatusCode::NOT_IMPLEMENTED, e);
        }
        Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
    };
    let identifier = match &route {
        Route::Base(id) | Route::Info(id) => id,
        Route::Image(request) => &request.identifier,
    }
    .clone();
    if let Route::Base(_) = route {
        let location = format!("{}/info.json", base_uri(&state.config, &identifier));
        return (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response();
    }
    let info_type = if matches!(&route, Route::Info(_)) {
        match info_media(headers) {
            Some(media) => Some(media),
            None => {
                return (
                    StatusCode::NOT_ACCEPTABLE,
                    [(header::VARY, "Accept")],
                    "unsupported Accept header",
                )
                    .into_response();
            }
        }
    } else {
        None
    };
    let generation = match state.storage.generation(&identifier).await {
        Ok(generation) => generation,
        Err(e) => return error_response(StatusCode::SERVICE_UNAVAILABLE, e),
    };
    let _derivative_guard = if let Route::Image(request) = &route {
        let key = state
            .storage
            .derivative_key(&identifier, &generation, raw_path);
        match state.storage.get_derivative(&key).await {
            Ok(Some(data)) => {
                return cached_image_response(
                    data,
                    request.format.mime(),
                    state.storage.metrics.clone(),
                );
            }
            Ok(None) => {}
            Err(e) => return error_response(StatusCode::SERVICE_UNAVAILABLE, e),
        }
        let digest = hash(&key);
        let index = usize::from(u8::from_str_radix(&digest[..2], 16).expect("hash prefix"));
        let guard = state.derivative_locks[index].lock().await;
        match state.storage.get_derivative(&key).await {
            Ok(Some(data)) => {
                return cached_image_response(
                    data,
                    request.format.mime(),
                    state.storage.metrics.clone(),
                );
            }
            Ok(None) => {}
            Err(e) => return error_response(StatusCode::SERVICE_UNAVAILABLE, e),
        }
        Some(guard)
    } else {
        None
    };
    let source = match state.storage.source_path(&identifier, &generation).await {
        Ok(path) => path,
        Err(e) if e.is::<SourceNotFound>() => return error_response(StatusCode::NOT_FOUND, e),
        Err(e) => return error_response(StatusCode::SERVICE_UNAVAILABLE, e),
    };
    let source_for_probe = source.path.clone();
    let (dimensions, is_jxl) = match tokio::task::spawn_blocking(move || {
        if jxl::is_jxl(&source_for_probe)? {
            Ok((jxl::metadata(&source_for_probe)?, true))
        } else {
            jp2::metadata(&source_for_probe).map(|dimensions| (dimensions, false))
        }
    })
    .await
    {
        Ok(Ok(probe)) => probe,
        Ok(Err(e)) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    match route {
        Route::Info(_) => {
            let media = info_type.expect("information media type was negotiated");
            let body = serde_json::to_vec(&info_document(
                &state.config,
                &identifier,
                dimensions.0,
                dimensions.1,
            ))
            .expect("serialize JSON");
            let content_type = match media {
                InfoMedia::JsonLd => {
                    "application/ld+json;profile=\"http://iiif.io/api/image/3/context.json\""
                }
                InfoMedia::Json => "application/json",
            };
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, content_type),
                    (header::VARY, "Accept"),
                ],
                body,
            )
                .into_response()
        }
        Route::Image(request) => {
            let region = match iiif::region_rect(&request.region, dimensions.0, dimensions.1) {
                Ok(region) => region,
                Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
            };
            let size =
                match iiif::output_size(&request.size, region, state.config.max_output_pixels) {
                    Ok(size) => size,
                    Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
                };
            let estimated_bytes = (u64::from(region.width) * u64::from(region.height))
                .min(state.config.max_decode_pixels)
                // The decoded region, the RGB copy, resizing, and encoding
                // can coexist. Charge a conservative peak per decoded pixel.
                // JPEG XL decodes through 32-bit float planes first; a lossy
                // 24 MP source measured about 27 bytes per pixel at peak.
                .saturating_mul(if is_jxl { 32 } else { 12 })
                .saturating_add(if !is_jxl {
                    // j2k's working memory follows the region it decodes, after
                    // discarding resolution levels. With j2k 0.13.1, 1024 px
                    // tiles at every scale, RGB and gray, peaked at no more than
                    // 3 bytes per decoded sample plus 2 MiB, output included.
                    let (width, height) =
                        pipeline::reduced_size(region, pipeline::reduction_level(region, size));
                    (u64::from(width) * u64::from(height))
                        .min(state.config.max_decode_pixels)
                        .saturating_mul(u64::from(dimensions.2) * 4)
                        .saturating_add(4 * 1_048_576)
                } else {
                    0
                });
            let needed = estimated_bytes.div_ceil(1_048_576).max(1);
            let available = state.config.max_temp_bitmap_bytes / 1_048_576;
            if needed > available {
                return text_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "temporary bitmap limit exceeded",
                );
            }
            let temp_permit = match state
                .temp_bitmaps
                .clone()
                .acquire_many_owned(needed as u32)
                .await
            {
                Ok(permit) => permit,
                Err(e) => return error_response(StatusCode::SERVICE_UNAVAILABLE, e),
            };
            let decode_permit = match state.decodes.acquire().await {
                Ok(permit) => permit,
                Err(e) => return error_response(StatusCode::SERVICE_UNAVAILABLE, e),
            };
            let started = Instant::now();
            let rendered = if is_jxl {
                pipeline::render_jxl(
                    &source.path,
                    region,
                    size,
                    &request,
                    state.config.encoding,
                    state.config.max_decode_pixels,
                )
                .await
            } else {
                pipeline::render_jp2(
                    &source.path,
                    region,
                    size,
                    &request,
                    state.config.encoding,
                    state.config.max_decode_pixels,
                )
                .await
            };
            drop(decode_permit);
            Metrics::increment(&state.storage.metrics.render_count);
            state.storage.metrics.render_duration_ns.fetch_add(
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            let data = match rendered {
                Ok(bytes) => {
                    Metrics::increment(if is_jxl {
                        &state.storage.metrics.jxl_renders
                    } else {
                        &state.storage.metrics.j2k_renders
                    });
                    bytes
                }
                Err(e) => {
                    error!(error = %e, "image render failed");
                    return error_response(StatusCode::INTERNAL_SERVER_ERROR, e);
                }
            };
            let key = state
                .storage
                .derivative_key(&identifier, &generation, raw_path);
            // Keep the reservation while S3 and the HTTP response still hold
            // references to the encoded image, including slow clients.
            let bytes = Bytes::from_owner(RenderedBytes {
                data,
                _permit: temp_permit,
            });
            match state.storage.generation(&identifier).await {
                Ok(current) if current == generation => {
                    if let Err(e) = state
                        .storage
                        .put_derivative(&key, bytes.clone(), request.format.mime())
                        .await
                    {
                        warn!(error = %e, "derivative cache write failed");
                    }
                }
                Ok(_) => {}
                Err(e) => warn!(error = %e, "could not check purge generation before cache write"),
            }
            image_response(bytes, request.format.mime())
        }
        Route::Base(_) => unreachable!(),
    }
}

fn image_response(data: Bytes, mime: &'static str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(mime))
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .header(header::CONTENT_LENGTH, data.len().to_string())
        .body(Body::from(data))
        .expect("valid image response")
}

fn cached_image_response(
    derivative: Derivative,
    mime: &'static str,
    metrics: Arc<Metrics>,
) -> Response {
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, HeaderValue::from_static(mime))
        .header(header::CACHE_CONTROL, "public, max-age=86400");
    if let Some(size) = derivative.size {
        response = response.header(header::CONTENT_LENGTH, size.to_string());
    }
    let stream = futures::stream::unfold(
        (derivative.body, metrics),
        |(mut body, metrics)| async move {
            body.next().await.map(|chunk| {
                if let Err(error) = &chunk {
                    Metrics::increment(&metrics.errors);
                    warn!(error = %error, "cached derivative stream failed");
                }
                (chunk, (body, metrics))
            })
        },
    );
    response
        .body(Body::from_stream(stream))
        .expect("valid cached image response")
}

#[cfg(test)]
mod tests {
    use super::{
        InfoMedia, RenderedBytes, iiif_path, image_response, info_media, requested_identifier,
    };
    use axum::http::{HeaderMap, header};
    use bytes::Bytes;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    #[test]
    fn iiif_prefix_matches_complete_path_segments() {
        assert_eq!(
            iiif_path("/iiif/v3/id/info.json", "/iiif/v3"),
            Some("/id/info.json")
        );
        assert_eq!(
            iiif_path("/iiif/id/info.json", "/iiif"),
            Some("/id/info.json")
        );
        assert_eq!(iiif_path("/id/info.json", ""), Some("/id/info.json"));
        assert_eq!(iiif_path("/iiif/v30/id/info.json", "/iiif/v3"), None);
        assert_eq!(iiif_path("/iiifx/id/info.json", "/iiif"), None);
        assert_eq!(iiif_path("/iiif/v3", "/iiif/v3"), None);
    }

    #[test]
    fn request_log_uses_decoded_iiif_identifier() {
        assert_eq!(
            requested_identifier("/images/v3/a%2Fb.jp2/info.json", "/images/v3"),
            Some("a/b.jp2".to_owned())
        );
        assert_eq!(
            requested_identifier("/images/v3/validator/full/max/0/default.jpg", "/images/v3"),
            Some("validator".to_owned())
        );
        assert_eq!(requested_identifier("/healthz", "/images/v3"), None);
        assert_eq!(requested_identifier("/admin/purge", ""), None);
    }

    #[tokio::test]
    async fn render_memory_reservation_lives_as_long_as_response_bytes() {
        let budget = Arc::new(Semaphore::new(1));
        let permit = budget.clone().acquire_owned().await.unwrap();
        let bytes = Bytes::from_owner(RenderedBytes {
            data: vec![1, 2, 3],
            _permit: permit,
        });
        let s3_copy = bytes.clone();
        let response = image_response(bytes, "image/jpeg");
        assert_eq!(budget.available_permits(), 0);
        assert_eq!(&s3_copy[..], &[1, 2, 3]);
        drop(s3_copy);
        assert_eq!(budget.available_permits(), 0);
        drop(response);
        assert_eq!(budget.available_permits(), 1);
    }

    #[test]
    fn negotiates_image_information_media_type() {
        assert_eq!(info_media(&HeaderMap::new()), Some(InfoMedia::JsonLd));
        for (accept, expected) in [
            ("application/json", Some(InfoMedia::Json)),
            ("application/ld+json", Some(InfoMedia::JsonLd)),
            ("*/*", Some(InfoMedia::JsonLd)),
            (
                "application/json;q=0.9, application/ld+json;q=0.5",
                Some(InfoMedia::Json),
            ),
            (
                "application/json, application/ld+json;q=0",
                Some(InfoMedia::Json),
            ),
            ("text/html", None),
            ("application/json;q=0", None),
            ("application/ld+json;q=0, */*;q=1", Some(InfoMedia::Json)),
            (
                "application/ld+json;profile=\"https://example.org/other\"",
                None,
            ),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::ACCEPT, accept.parse().unwrap());
            assert_eq!(info_media(&headers), expected, "{accept}");
        }
    }
}
