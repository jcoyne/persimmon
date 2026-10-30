// Handler steps return the early-exit `Response` as their error. It is large,
// but a request builds at most one, so boxing it would gain nothing.
#![allow(clippy::result_large_err)]

use std::{path::Path, sync::Arc, time::Instant};

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
    iiif::{self, ImageRequest, Rect, Route},
    jp2, jxl,
    metrics::Metrics,
    pipeline,
    storage::{Derivative, SourceLease, SourceNotFound, hash},
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
    let mut response = serve(&state, uri.path(), &headers)
        .await
        .unwrap_or_else(|response| response);
    if response.status().is_server_error() {
        Metrics::increment(&counters.errors);
    }
    if method == Method::HEAD {
        *response.body_mut() = Body::empty();
    }
    response
}

/// Each step returns `Err` with the response to send when the request stops early.
type Step<T> = Result<T, Response>;

async fn serve(state: &AppState, raw_path: &str, headers: &HeaderMap) -> Step<Response> {
    match parse_request(raw_path, &state.config.route_prefix)? {
        Route::Base(identifier) => Ok(redirect_to_info(&state.config, &identifier)),
        Route::Info(identifier) => serve_info(state, &identifier, headers).await,
        Route::Image(request) => serve_image(state, raw_path, &request).await,
    }
}

fn parse_request(raw_path: &str, route_prefix: &str) -> Step<Route> {
    let path = iiif_path(raw_path, route_prefix)
        .ok_or_else(|| text_response(StatusCode::NOT_FOUND, "Not found"))?;
    iiif::parse_route(path).map_err(|e| {
        let status = if e.0 == "invalid IIIF route" {
            StatusCode::NOT_FOUND
        } else if e.0 == "upscaling is not supported" {
            StatusCode::NOT_IMPLEMENTED
        } else {
            StatusCode::BAD_REQUEST
        };
        error_response(status, e)
    })
}

fn redirect_to_info(config: &Config, identifier: &str) -> Response {
    let location = format!("{}/info.json", base_uri(config, identifier));
    (StatusCode::SEE_OTHER, [(header::LOCATION, location)]).into_response()
}

async fn serve_info(state: &AppState, identifier: &str, headers: &HeaderMap) -> Step<Response> {
    let media = info_media(headers).ok_or_else(|| {
        (
            StatusCode::NOT_ACCEPTABLE,
            [(header::VARY, "Accept")],
            "unsupported Accept header",
        )
            .into_response()
    })?;
    let generation = current_generation(state, identifier).await?;
    let source = fetch_source(state, identifier, &generation).await?;
    let ((width, height, _), _) = probe_source(&source.path).await?;
    let body = serde_json::to_vec(&info_document(&state.config, identifier, width, height))
        .expect("serialize JSON");
    let content_type = match media {
        InfoMedia::JsonLd => {
            "application/ld+json;profile=\"http://iiif.io/api/image/3/context.json\""
        }
        InfoMedia::Json => "application/json",
    };
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::VARY, "Accept"),
        ],
        body,
    )
        .into_response())
}

async fn serve_image(state: &AppState, raw_path: &str, request: &ImageRequest) -> Step<Response> {
    let identifier = &request.identifier;
    let mime = request.format.mime();
    let generation = current_generation(state, identifier).await?;
    let key = state
        .storage
        .derivative_key(identifier, &generation, raw_path);
    if let Some(response) = cached_derivative(state, &key, mime).await? {
        return Ok(response);
    }
    // Concurrent requests for the same derivative wait here, so only the
    // first renders it and the rest find it in the cache.
    let digest = hash(&key);
    let index = usize::from(u8::from_str_radix(&digest[..2], 16).expect("hash prefix"));
    let _derivative_guard = state.derivative_locks[index].lock().await;
    if let Some(response) = cached_derivative(state, &key, mime).await? {
        return Ok(response);
    }
    let source = fetch_source(state, identifier, &generation).await?;
    let ((width, height, components), is_jxl) = probe_source(&source.path).await?;
    let region = iiif::region_rect(&request.region, width, height)
        .map_err(|e| error_response(StatusCode::BAD_REQUEST, e))?;
    let size = iiif::output_size(&request.size, region, state.config.max_output_pixels)
        .map_err(|e| error_response(StatusCode::BAD_REQUEST, e))?;
    let temp_permit = reserve_render_memory(state, region, size, components, is_jxl).await?;
    let data = render(state, &source.path, region, size, request, is_jxl).await?;
    // Keep the reservation while S3 and the HTTP response still hold
    // references to the encoded image, including slow clients.
    let bytes = Bytes::from_owner(RenderedBytes {
        data,
        _permit: temp_permit,
    });
    store_derivative(state, identifier, &generation, &key, bytes.clone(), mime).await;
    Ok(image_response(bytes, mime))
}

async fn current_generation(state: &AppState, identifier: &str) -> Step<String> {
    state
        .storage
        .generation(identifier)
        .await
        .map_err(|e| error_response(StatusCode::SERVICE_UNAVAILABLE, e))
}

async fn fetch_source(state: &AppState, identifier: &str, generation: &str) -> Step<SourceLease> {
    state
        .storage
        .source_path(identifier, generation)
        .await
        .map_err(|e| {
            let status = if e.is::<SourceNotFound>() {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            error_response(status, e)
        })
}

/// Reads the source's width, height and component count, and whether it is JPEG XL.
async fn probe_source(path: &Path) -> Step<((u32, u32, u16), bool)> {
    let path = path.to_owned();
    let probe = tokio::task::spawn_blocking(move || {
        if jxl::is_jxl(&path)? {
            Ok((jxl::metadata(&path)?, true))
        } else {
            jp2::metadata(&path).map(|dimensions| (dimensions, false))
        }
    })
    .await;
    match probe {
        Ok(Ok(probe)) => Ok(probe),
        Ok(Err(e)) => Err(error_response(StatusCode::INTERNAL_SERVER_ERROR, e)),
        Err(e) => Err(error_response(StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

async fn cached_derivative(
    state: &AppState,
    key: &str,
    mime: &'static str,
) -> Step<Option<Response>> {
    match state.storage.get_derivative(key).await {
        Ok(derivative) => {
            Ok(derivative
                .map(|data| cached_image_response(data, mime, state.storage.metrics.clone())))
        }
        Err(e) => Err(error_response(StatusCode::SERVICE_UNAVAILABLE, e)),
    }
}

async fn reserve_render_memory(
    state: &AppState,
    region: Rect,
    size: (u32, u32),
    components: u16,
    is_jxl: bool,
) -> Step<OwnedSemaphorePermit> {
    let needed = render_memory_mib(&state.config, region, size, components, is_jxl);
    let available = state.config.max_temp_bitmap_bytes / 1_048_576;
    if needed > available {
        return Err(text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "temporary bitmap limit exceeded",
        ));
    }
    state
        .temp_bitmaps
        .clone()
        .acquire_many_owned(needed as u32)
        .await
        .map_err(|e| error_response(StatusCode::SERVICE_UNAVAILABLE, e))
}

/// Estimates the peak memory of one render, in whole MiB.
fn render_memory_mib(
    config: &Config,
    region: Rect,
    size: (u32, u32),
    components: u16,
    is_jxl: bool,
) -> u64 {
    let estimated_bytes = (u64::from(region.width) * u64::from(region.height))
        .min(config.max_decode_pixels)
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
                .min(config.max_decode_pixels)
                .saturating_mul(u64::from(components) * 4)
                .saturating_add(4 * 1_048_576)
        } else {
            0
        });
    estimated_bytes.div_ceil(1_048_576).max(1)
}

async fn render(
    state: &AppState,
    source: &Path,
    region: Rect,
    size: (u32, u32),
    request: &ImageRequest,
    is_jxl: bool,
) -> Step<Vec<u8>> {
    let decode_permit = state
        .decodes
        .acquire()
        .await
        .map_err(|e| error_response(StatusCode::SERVICE_UNAVAILABLE, e))?;
    let started = Instant::now();
    let rendered = if is_jxl {
        pipeline::render_jxl(
            source,
            region,
            size,
            request,
            state.config.encoding,
            state.config.max_decode_pixels,
        )
        .await
    } else {
        pipeline::render_jp2(
            source,
            region,
            size,
            request,
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
    match rendered {
        Ok(bytes) => {
            Metrics::increment(if is_jxl {
                &state.storage.metrics.jxl_renders
            } else {
                &state.storage.metrics.j2k_renders
            });
            Ok(bytes)
        }
        Err(e) => {
            error!(error = %e, "image render failed");
            Err(error_response(StatusCode::INTERNAL_SERVER_ERROR, e))
        }
    }
}

/// Caches the derivative unless the source was purged while it rendered.
async fn store_derivative(
    state: &AppState,
    identifier: &str,
    generation: &str,
    key: &str,
    bytes: Bytes,
    mime: &'static str,
) {
    match state.storage.generation(identifier).await {
        Ok(current) if current == generation => {
            if let Err(e) = state.storage.put_derivative(key, bytes, mime).await {
                warn!(error = %e, "derivative cache write failed");
            }
        }
        Ok(_) => {}
        Err(e) => warn!(error = %e, "could not check purge generation before cache write"),
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
