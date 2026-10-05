use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::{
    Client,
    error::{DisplayErrorContext, ProvideErrorMetadata, SdkError},
    operation::list_objects_v2::{ListObjectsV2Error, ListObjectsV2Output},
};
use axum::{
    Router,
    body::Body,
    extract::{Json, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use persimmon::{
    config::{Config, PruneConfig},
    iiif::{self, Route},
    jp2,
    metrics::Metrics,
    pipeline::{KakaduBackend, KakaduCli, KakaduNative, RenderPath},
    storage::{Derivative, SourceNotFound, Storage, hash, prune_derivatives},
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tower_http::{
    cors::CorsLayer,
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{Level, error, info, warn};

struct AppState {
    config: Arc<Config>,
    storage: Arc<Storage>,
    kakadu: KakaduBackend,
    decodes: Semaphore,
    temp_bitmaps: Arc<Semaphore>,
    derivative_locks: Vec<Mutex<()>>,
    health: Mutex<Option<(Instant, HealthReport)>>,
}

struct RenderedBytes {
    data: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

impl AsRef<[u8]> for RenderedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

fn text_response(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message.to_owned(),
    )
        .into_response()
}

fn error_response(status: StatusCode, error: impl std::fmt::Display) -> Response {
    if status.is_server_error() {
        error!(status = status.as_u16(), error = %error, "request failed");
        text_response(status, status.canonical_reason().unwrap_or("server error"))
    } else {
        text_response(status, &error.to_string())
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

const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Serialize)]
struct HealthCheck {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl HealthCheck {
    fn ok() -> Self {
        Self {
            status: "ok",
            error: None,
        }
    }

    fn failed(error: impl Into<String>) -> Self {
        Self {
            status: "error",
            error: Some(error.into()),
        }
    }

    fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

#[derive(Clone, Serialize)]
struct HealthChecks {
    cache_bucket: HealthCheck,
    source_bucket: HealthCheck,
    kakadu: HealthCheck,
}

#[derive(Clone, Serialize)]
struct HealthReport {
    status: &'static str,
    checks: HealthChecks,
}

impl HealthReport {
    fn new(checks: HealthChecks) -> Self {
        let healthy =
            checks.cache_bucket.is_ok() && checks.source_bucket.is_ok() && checks.kakadu.is_ok();
        Self {
            status: if healthy { "ok" } else { "unavailable" },
            checks,
        }
    }

    fn is_ok(&self) -> bool {
        self.status == "ok"
    }

    /// Healthy responses keep the plain `OK` body that existing probes expect;
    /// failures return the JSON report so the failed check is visible.
    fn response(&self) -> Response {
        let mut response = if self.is_ok() {
            text_response(StatusCode::OK, "OK")
        } else {
            (StatusCode::SERVICE_UNAVAILABLE, Json(self.clone())).into_response()
        };
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

/// Summarizes an S3 failure for the public health response. The full error,
/// which may include endpoints and request IDs, goes only to the log.
fn s3_error_summary(error: &SdkError<ListObjectsV2Error, HttpResponse>) -> String {
    match error {
        SdkError::ServiceError(e) => e
            .err()
            .code()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("HTTP {}", e.raw().status().as_u16())),
        SdkError::TimeoutError(_) => "S3 request timed out".into(),
        SdkError::DispatchFailure(_) => "could not connect to S3".into(),
        SdkError::ResponseError(_) => "invalid response from S3".into(),
        _ => "S3 request failed".into(),
    }
}

async fn check_bucket(
    name: &'static str,
    request: impl Future<
        Output = Result<ListObjectsV2Output, SdkError<ListObjectsV2Error, HttpResponse>>,
    >,
) -> HealthCheck {
    match tokio::time::timeout(HEALTH_CHECK_TIMEOUT, request).await {
        Ok(Ok(_)) => HealthCheck::ok(),
        Ok(Err(e)) => {
            warn!(check = name, error = %DisplayErrorContext(&e), "health check failed");
            HealthCheck::failed(s3_error_summary(&e))
        }
        Err(_) => {
            warn!(check = name, "health check timed out");
            HealthCheck::failed("timed out")
        }
    }
}

async fn check_kakadu(kakadu: &KakaduBackend) -> HealthCheck {
    match tokio::time::timeout(HEALTH_CHECK_TIMEOUT, kakadu.verify_version()).await {
        Ok(Ok(())) => HealthCheck::ok(),
        Ok(Err(e)) => {
            warn!(check = "kakadu", error = %format!("{e:#}"), "health check failed");
            HealthCheck::failed(e.to_string())
        }
        Err(_) => {
            warn!(check = "kakadu", "health check timed out");
            HealthCheck::failed("timed out")
        }
    }
}

async fn health(State(state): State<Arc<AppState>>) -> Response {
    let mut cached = state.health.lock().await;
    if let Some((checked, report)) = &*cached
        && checked.elapsed() < Duration::from_secs(5)
    {
        return report.response();
    }
    let cache = state
        .storage
        .s3
        .list_objects_v2()
        .bucket(&state.config.cache_bucket)
        .prefix(&state.config.cache_prefix)
        .max_keys(1)
        .send();
    let source = state
        .storage
        .s3
        .list_objects_v2()
        .bucket(&state.config.source_bucket)
        .max_keys(1)
        .send();
    state
        .storage
        .metrics
        .s3_requests
        .fetch_add(2, std::sync::atomic::Ordering::Relaxed);
    let (cache_bucket, source_bucket, kakadu) = tokio::join!(
        check_bucket("cache_bucket", cache),
        check_bucket("source_bucket", source),
        check_kakadu(&state.kakadu),
    );
    let report = HealthReport::new(HealthChecks {
        cache_bucket,
        source_bucket,
        kakadu,
    });
    if !report.is_ok() {
        Metrics::increment(&state.storage.metrics.errors);
    }
    let response = report.response();
    *cached = Some((Instant::now(), report));
    response
}

async fn metrics(State(state): State<Arc<AppState>>) -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        state.storage.metrics.snapshot(),
    )
        .into_response()
}

#[derive(Deserialize)]
struct PurgeInput {
    identifier: String,
}

#[derive(Serialize)]
struct PurgeResult {
    identifier: String,
    generation: String,
}

fn authenticated(headers: &HeaderMap, config: &Config) -> bool {
    let Some(encoded) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
    else {
        return false;
    };
    let Ok(decoded) = STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(credentials) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((user, password)) = credentials.split_once(':') else {
        return false;
    };
    bool::from(user.as_bytes().ct_eq(config.admin_user.as_bytes()))
        & bool::from(password.as_bytes().ct_eq(config.admin_password.as_bytes()))
}

async fn purge(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<PurgeInput>,
) -> Response {
    if !authenticated(&headers, &state.config) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"persimmon-admin\"")],
            "Unauthorized",
        )
            .into_response();
    }
    if input.identifier.is_empty()
        || input.identifier.contains('\0')
        || input.identifier.starts_with('/')
    {
        return text_response(StatusCode::BAD_REQUEST, "invalid identifier");
    }
    match state.storage.purge(&input.identifier).await {
        Ok(generation) => {
            let storage = state.storage.clone();
            let identifier = input.identifier.clone();
            let keep = generation.clone();
            tokio::spawn(async move {
                if let Err(e) = storage.delete_old_derivatives(&identifier, &keep).await {
                    warn!(error = %e, "derivative purge cleanup failed");
                }
            });
            Json(PurgeResult {
                identifier: input.identifier,
                generation,
            })
            .into_response()
        }
        Err(e) => {
            Metrics::increment(&state.storage.metrics.errors);
            error_response(StatusCode::SERVICE_UNAVAILABLE, e)
        }
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
        "preferredFormats": ["webp"],
        "extraFormats": ["webp"],
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

async fn iiif_handler(
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
    let dimensions =
        match tokio::task::spawn_blocking(move || jp2::metadata(&source_for_probe)).await {
            Ok(Ok(dimensions)) => dimensions,
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
                // Kakadu's packed output, the RGB copy, resizing, and encoding
                // can coexist. Charge a conservative peak per decoded pixel.
                .saturating_mul(12);
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
            let rendered = state
                .kakadu
                .render(
                    &source.path,
                    (dimensions.0, dimensions.1),
                    region,
                    size,
                    &request,
                )
                .await;
            drop(decode_permit);
            Metrics::increment(&state.storage.metrics.render_count);
            state.storage.metrics.render_duration_ns.fetch_add(
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
            let data = match rendered {
                Ok(rendered) => {
                    let counter = match rendered.path {
                        RenderPath::Native => &state.storage.metrics.native_renders,
                        RenderPath::Command => &state.storage.metrics.command_renders,
                        RenderPath::CommandFallback => {
                            &state.storage.metrics.command_fallback_renders
                        }
                    };
                    Metrics::increment(counter);
                    rendered.bytes
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();
    let command = std::env::args().nth(1);
    if matches!(command.as_deref(), Some("prune-cache" | "prune-cache-loop")) {
        let config = PruneConfig::from_env()?;
        let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
        let mut s3_config = aws_sdk_s3::config::Builder::from(&aws);
        if let Some(endpoint) = &config.s3_endpoint {
            s3_config = s3_config.endpoint_url(endpoint).force_path_style(true);
        }
        let s3 = Client::from_conf(s3_config.build());
        loop {
            let (bytes, deleted) = prune_derivatives(
                &s3,
                &config.cache_bucket,
                &config.cache_prefix,
                config.derivative_cache_limit,
                None,
            )
            .await?;
            info!(bytes, deleted, "derivative cleanup complete");
            if command.as_deref() == Some("prune-cache") {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(config.prune_interval_seconds)) => {},
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        return Ok(());
    }
    let config = Arc::new(Config::from_env()?);
    let aws = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let mut s3_config = aws_sdk_s3::config::Builder::from(&aws);
    if let Some(endpoint) = &config.s3_endpoint {
        s3_config = s3_config.endpoint_url(endpoint).force_path_style(true);
    }
    let s3 = Client::from_conf(s3_config.build());
    let storage = Arc::new(Storage::new(s3, config.clone()).await?);
    let command = KakaduCli {
        executable: config.kakadu_expand.clone(),
        temp_dir: config.local_cache_dir.clone(),
        max_decode_pixels: config.max_decode_pixels,
    };
    let kakadu = if let Some(path) = &config.kakadu_native {
        KakaduBackend::Native {
            native: KakaduNative::load(path, config.max_native_decode_pixels)?,
            fallback: command,
        }
    } else {
        KakaduBackend::Command(command)
    };
    kakadu
        .verify_version()
        .await
        .context("Kakadu startup check")?;
    let state = Arc::new(AppState {
        decodes: Semaphore::new(config.max_parallel_decodes),
        temp_bitmaps: Arc::new(Semaphore::new(
            (config.max_temp_bitmap_bytes / 1_048_576) as usize,
        )),
        derivative_locks: (0..256).map(|_| Mutex::new(())).collect(),
        health: Mutex::new(None),
        config: config.clone(),
        storage,
        kakadu,
    });
    let app = Router::new()
        .route("/healthz", get(health))
        .route("/metrics", get(metrics))
        .route("/admin/purge", post(purge))
        .fallback(iiif_handler)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http().on_response(DefaultOnResponse::new().level(Level::INFO)))
        .with_state(state);
    info!(listen = %config.listen, "persimmon starting");
    if let (Some(cert), Some(key)) = (&config.tls_cert, &config.tls_key) {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
        axum_server::bind_rustls(config.listen, tls)
            .serve(app.into_make_service())
            .await?;
    } else {
        axum_server::bind(config.listen)
            .serve(app.into_make_service())
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        HealthCheck, HealthChecks, HealthReport, InfoMedia, RenderedBytes, iiif_path,
        image_response, info_media,
    };
    use axum::http::{HeaderMap, StatusCode, header};
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

    #[test]
    fn health_report_names_failed_check() {
        let report = HealthReport::new(HealthChecks {
            cache_bucket: HealthCheck::ok(),
            source_bucket: HealthCheck::failed("AccessDenied"),
            kakadu: HealthCheck::ok(),
        });
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::json!({
                "status": "unavailable",
                "checks": {
                    "cache_bucket": {"status": "ok"},
                    "source_bucket": {"status": "error", "error": "AccessDenied"},
                    "kakadu": {"status": "ok"}
                }
            })
        );
        assert_eq!(report.response().status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn healthy_report_is_ok() {
        let report = HealthReport::new(HealthChecks {
            cache_bucket: HealthCheck::ok(),
            source_bucket: HealthCheck::ok(),
            kakadu: HealthCheck::ok(),
        });
        let response = report.response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        let body =
            futures::executor::block_on(axum::body::to_bytes(response.into_body(), 64)).unwrap();
        assert_eq!(body, "OK");
    }
}
