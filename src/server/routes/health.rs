use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::{
    error::{DisplayErrorContext, ProvideErrorMetadata, SdkError},
    operation::list_objects_v2::{ListObjectsV2Error, ListObjectsV2Output},
};
use axum::{
    extract::{Json, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use tracing::warn;

use crate::metrics::Metrics;
use crate::server::{AppState, response::text_response};

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
}

#[derive(Clone, Serialize)]
pub(crate) struct HealthReport {
    status: &'static str,
    checks: HealthChecks,
}

impl HealthReport {
    fn new(checks: HealthChecks) -> Self {
        let healthy = checks.cache_bucket.is_ok() && checks.source_bucket.is_ok();
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

pub(crate) async fn health(State(state): State<Arc<AppState>>) -> Response {
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
    let (cache_bucket, source_bucket) = tokio::join!(
        check_bucket("cache_bucket", cache),
        check_bucket("source_bucket", source),
    );
    let report = HealthReport::new(HealthChecks {
        cache_bucket,
        source_bucket,
    });
    if !report.is_ok() {
        Metrics::increment(&state.storage.metrics.errors);
    }
    let response = report.response();
    *cached = Some((Instant::now(), report));
    response
}

#[cfg(test)]
mod tests {
    use super::{HealthCheck, HealthChecks, HealthReport};
    use axum::http::{StatusCode, header};

    #[test]
    fn health_report_names_failed_check() {
        let report = HealthReport::new(HealthChecks {
            cache_bucket: HealthCheck::ok(),
            source_bucket: HealthCheck::failed("AccessDenied"),
        });
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::json!({
                "status": "unavailable",
                "checks": {
                    "cache_bucket": {"status": "ok"},
                    "source_bucket": {"status": "error", "error": "AccessDenied"}
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
