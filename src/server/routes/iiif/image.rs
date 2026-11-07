use std::{path::Path, sync::Arc, time::Instant};

use axum::{
    body::Body,
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use bytes::Bytes;
use tokio::sync::OwnedSemaphorePermit;
use tracing::warn;

use crate::{
    config::Config,
    iiif::{self, ImageRequest, Rect},
    metrics::Metrics,
    pipeline,
    server::{
        AppState,
        response::{error_response, text_response},
    },
    storage::{Derivative, hash},
};

use super::{PUBLIC_CACHE_CONTROL, Step, source};

struct RenderedBytes {
    data: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

impl AsRef<[u8]> for RenderedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

pub(super) async fn serve(
    state: &AppState,
    raw_path: &str,
    request: &ImageRequest,
) -> Step<Response> {
    let identifier = &request.identifier;
    let mime = request.format.mime();
    let generation = source::current_cache_generation(state, identifier).await?;
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
    let source = source::fetch(state, identifier, &generation).await?;
    let ((width, height, components), is_jxl) = source::probe(&source.path).await?;
    let region = iiif::region_rect(&request.region, width, height)
        .map_err(|e| error_response(StatusCode::BAD_REQUEST, e))?;
    let size = iiif::output_size(&request.size, region, state.config.max_output_pixels)
        .map_err(|e| error_response(StatusCode::BAD_REQUEST, e))?;
    let temp_permit = reserve_render_memory(state, region, size, components, is_jxl).await?;
    let data = render(state, &source.path, region, size, request, is_jxl).await?;
    // Release the source before the cache write, so it can be evicted while
    // S3 and the client receive the image.
    drop(source);
    // Keep the reservation while S3 and the HTTP response still hold
    // references to the encoded image, including slow clients.
    let bytes = Bytes::from_owner(RenderedBytes {
        data,
        _permit: temp_permit,
    });
    store_derivative(state, identifier, &generation, &key, bytes.clone(), mime).await;
    Ok(image_response(bytes, mime))
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
        Err(e) => Err(error_response(StatusCode::INTERNAL_SERVER_ERROR, e)),
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
        .header(header::CACHE_CONTROL, PUBLIC_CACHE_CONTROL)
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
        .header(header::CACHE_CONTROL, PUBLIC_CACHE_CONTROL);
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
    use std::sync::Arc;

    use bytes::Bytes;
    use tokio::sync::Semaphore;

    use super::{RenderedBytes, image_response};

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
}
