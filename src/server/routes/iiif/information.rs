use std::sync::Arc;

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use tracing::warn;

use crate::{
    config::Config,
    metrics::Metrics,
    server::{AppState, response::error_response},
    storage::{Derivative, hash},
};

use super::{PUBLIC_CACHE_CONTROL, Step, base_uri, source};

// Increment this when the serialized information document changes in a way
// not represented by the configuration values in information_cache_variant.
const INFORMATION_CACHE_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InfoMedia {
    JsonLd,
    Json,
}

/// Selects the supported information representation preferred by the request's Accept header.
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

/// Generates advertised IIIF sizes as power-of-two reductions within the configured bounds.
fn info_sizes(width: u32, height: u32, min_size: u32, max_pixels: u64) -> Vec<serde_json::Value> {
    let full_area = u64::from(width) * u64::from(height);

    // Compute the lower bound by repeatedly quartering the full image area.
    // This keeps every advertised size on the source image's power-of-two
    // series instead of first scaling to an arbitrary max area.
    let mut min_reduction = 0u32;
    let mut area_divisor = 1u128;
    while u128::from(full_area) > u128::from(max_pixels) * area_divisor {
        min_reduction += 1;
        area_divisor *= 4;
    }

    // The full size is always eligible at this end of the range. Additional
    // halvings are eligible while the smaller unrounded dimension remains at
    // least min_size.
    let mut max_reduction = 0u32;
    let smaller_dimension = u64::from(width.min(height));
    while smaller_dimension >= u64::from(min_size) * (1u64 << (max_reduction + 1)) {
        max_reduction += 1;
    }

    if min_reduction > max_reduction {
        return Vec::new();
    }

    (min_reduction..=max_reduction)
        .rev()
        .map(|reduction| {
            let divisor = 1u64 << reduction;
            // Round positive halves up directly from the full dimensions so
            // odd dimensions scale consistently at every reduction factor.
            let rounded_width = (u64::from(width) + divisor / 2) / divisor;
            let rounded_height = (u64::from(height) + divisor / 2) / divisor;
            serde_json::json!({"width": rounded_width, "height": rounded_height})
        })
        .collect()
}

/// Builds the IIIF Image API 3 information document for the source dimensions.
fn info_document(config: &Config, identifier: &str, width: u32, height: u32) -> serde_json::Value {
    let sizes = info_sizes(width, height, config.min_size, config.max_output_pixels);
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
        "sizes": sizes,
        "preferredFormats": ["webp", "avif", "jpg"],
        "extraFormats": ["webp", "avif", "jxl"],
        "extraQualities": ["color", "gray"]
    });
    if width.max(height) >= tile_width
        && u64::from(tile_width) * u64::from(tile_width) <= config.max_output_pixels
    {
        info["tiles"] = serde_json::json!([{"width": tile_width, "scaleFactors": factors}]);
    }
    info
}

/// Identifies the configuration-dependent variant of a cached information document.
fn information_cache_variant(config: &Config) -> String {
    format!(
        "info-v{}\0{}\0{}\0{}\0{}\0{}.json",
        INFORMATION_CACHE_VERSION,
        config.public_base_url,
        config.iiif_prefix,
        config.max_output_pixels,
        config.min_size,
        config.min_tile_size,
    )
}

/// Serves an information request from cache or builds and caches it on a miss.
pub(super) async fn serve(
    state: &AppState,
    identifier: &str,
    headers: &HeaderMap,
) -> Step<Response> {
    let media = info_media(headers).ok_or_else(|| {
        (
            StatusCode::NOT_ACCEPTABLE,
            [(header::VARY, "Accept")],
            "unsupported Accept header",
        )
            .into_response()
    })?;
    let generation = source::current_generation(state, identifier).await?;
    let key = state.storage.derivative_key(
        identifier,
        &generation,
        &information_cache_variant(&state.config),
    );
    if let Some(response) = cached_information(state, &key, media).await? {
        return Ok(response);
    }
    // Serialize once when concurrent requests miss the same information
    // document. JSON and JSON-LD share bytes and negotiate only their media
    // type, so they can safely use the same cache entry.
    let digest = hash(&key);
    let index = usize::from(u8::from_str_radix(&digest[..2], 16).expect("hash prefix"));
    let _derivative_guard = state.derivative_locks[index].lock().await;
    if let Some(response) = cached_information(state, &key, media).await? {
        return Ok(response);
    }
    let source = source::fetch(state, identifier, &generation).await?;
    let ((width, height, _), _) = source::probe(&source.path).await?;
    let bytes = Bytes::from(
        serde_json::to_vec(&info_document(&state.config, identifier, width, height))
            .expect("serialize JSON"),
    );
    store_information(state, identifier, &generation, &key, bytes.clone()).await;
    Ok(information_response(bytes, media))
}

/// Returns the response Content-Type for the negotiated information representation.
fn content_type(media: InfoMedia) -> &'static str {
    match media {
        InfoMedia::JsonLd => {
            "application/ld+json;profile=\"http://iiif.io/api/image/3/context.json\""
        }
        InfoMedia::Json => "application/json",
    }
}

/// Loads a cached information document and turns it into a negotiated HTTP response.
async fn cached_information(
    state: &AppState,
    key: &str,
    media: InfoMedia,
) -> Step<Option<Response>> {
    match state.storage.get_derivative(key).await {
        Ok(cached) => Ok(cached
            .map(|data| cached_information_response(data, media, state.storage.metrics.clone()))),
        Err(e) => Err(error_response(StatusCode::SERVICE_UNAVAILABLE, e)),
    }
}

/// Caches the information document unless the source was purged while it was built.
async fn store_information(
    state: &AppState,
    identifier: &str,
    generation: &str,
    key: &str,
    bytes: Bytes,
) {
    match state.storage.generation(identifier).await {
        Ok(current) if current == generation => {
            if let Err(e) = state
                .storage
                .put_derivative(key, bytes, "application/json")
                .await
            {
                warn!(error = %e, "information cache write failed");
            }
        }
        Ok(_) => {}
        Err(e) => {
            warn!(error = %e, "could not check purge generation before information cache write")
        }
    }
}

/// Builds a complete HTTP response around a newly generated information document.
fn information_response(data: Bytes, media: InfoMedia) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static(content_type(media)),
        )
        .header(header::VARY, "Accept")
        .header(header::CACHE_CONTROL, PUBLIC_CACHE_CONTROL)
        .header(header::CONTENT_LENGTH, data.len().to_string())
        .body(Body::from(data))
        .expect("valid information response")
}

/// Streams a cached information document with the same headers as a fresh response.
fn cached_information_response(
    cached: Derivative,
    media: InfoMedia,
    metrics: Arc<Metrics>,
) -> Response {
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static(content_type(media)),
        )
        .header(header::VARY, "Accept")
        .header(header::CACHE_CONTROL, PUBLIC_CACHE_CONTROL);
    if let Some(size) = cached.size {
        response = response.header(header::CONTENT_LENGTH, size.to_string());
    }
    let stream =
        futures::stream::unfold((cached.body, metrics), |(mut body, metrics)| async move {
            body.next().await.map(|chunk| {
                if let Err(error) = &chunk {
                    Metrics::increment(&metrics.errors);
                    warn!(error = %error, "cached information stream failed");
                }
                (chunk, (body, metrics))
            })
        });
    response
        .body(Body::from_stream(stream))
        .expect("valid cached information response")
}

#[cfg(test)]
mod tests {
    use super::{InfoMedia, info_media, info_sizes, information_response};
    use axum::http::{HeaderMap, header};
    use bytes::Bytes;

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

    #[test]
    fn information_sizes_form_a_power_of_two_series() {
        assert_eq!(
            info_sizes(1500, 1200, 64, 100_000_000),
            vec![
                serde_json::json!({"width": 94, "height": 75}),
                serde_json::json!({"width": 188, "height": 150}),
                serde_json::json!({"width": 375, "height": 300}),
                serde_json::json!({"width": 750, "height": 600}),
                serde_json::json!({"width": 1500, "height": 1200}),
            ]
        );
    }

    #[test]
    fn information_sizes_apply_upper_and_lower_reduction_bounds() {
        assert_eq!(
            info_sizes(1500, 1200, 64, 112_500),
            vec![
                serde_json::json!({"width": 94, "height": 75}),
                serde_json::json!({"width": 188, "height": 150}),
                serde_json::json!({"width": 375, "height": 300}),
            ]
        );
        assert!(info_sizes(1500, 1200, 64, 1_000).is_empty());
    }

    #[test]
    fn information_sizes_round_each_size_from_the_full_dimensions() {
        assert_eq!(
            info_sizes(5, 5, 1, 100),
            vec![
                serde_json::json!({"width": 1, "height": 1}),
                serde_json::json!({"width": 3, "height": 3}),
                serde_json::json!({"width": 5, "height": 5}),
            ]
        );
        assert_eq!(
            info_sizes(32, 16, 64, 1_000),
            vec![serde_json::json!({"width": 32, "height": 16})]
        );
    }

    #[test]
    fn information_response_is_publicly_cacheable_and_varies_on_accept() {
        let response = information_response(Bytes::from_static(b"{}"), InfoMedia::Json);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(response.headers()[header::VARY], "Accept");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "public, max-age=86400"
        );
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "2");
    }
}
