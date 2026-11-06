use std::{sync::Arc, time::Instant};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use tokio::sync::{Mutex, Semaphore, SemaphorePermit};
use tower_http::cors::CorsLayer;
use tracing::{Instrument, info};

use crate::{config::Config, storage::Storage};
use routes::{health::HealthReport, iiif::requested_identifier};

mod response;
mod routes;

pub struct AppState {
    config: Arc<Config>,
    storage: Arc<Storage>,
    decodes: Semaphore,
    requests: Semaphore,
    temp_bitmaps: Arc<Semaphore>,
    derivative_locks: Vec<Mutex<()>>,
    health: Mutex<Option<(Instant, HealthReport)>>,
}

impl AppState {
    pub fn new(config: Arc<Config>, storage: Arc<Storage>) -> Self {
        Self {
            decodes: Semaphore::new(config.max_parallel_decodes),
            requests: Semaphore::new(config.max_in_flight_requests),
            temp_bitmaps: Arc::new(Semaphore::new(
                (config.max_temp_bitmap_bytes / 1_048_576) as usize,
            )),
            derivative_locks: (0..256).map(|_| Mutex::new(())).collect(),
            health: Mutex::new(None),
            config,
            storage,
        }
    }

    /// Admits work without adding an unbounded waiter to an internal queue.
    pub(crate) fn try_admit(&self) -> Option<SemaphorePermit<'_>> {
        try_permit(&self.requests)
    }
}

fn try_permit(semaphore: &Semaphore) -> Option<SemaphorePermit<'_>> {
    semaphore.try_acquire().ok()
}

pub(crate) fn overloaded_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::RETRY_AFTER, "1")],
        "Server is busy; retry later",
    )
        .into_response()
}

pub fn router(state: Arc<AppState>) -> Router {
    let config = state.config.clone();
    Router::new()
        .route("/healthz", get(routes::health::health))
        .route("/livez", get(routes::health::live))
        .route("/metrics", get(routes::metrics::metrics))
        .route("/admin/purge", post(routes::purge::purge))
        .fallback(routes::iiif::iiif_handler)
        .layer(CorsLayer::permissive())
        .layer(middleware::from_fn_with_state(config, log_request))
        .with_state(state)
}

async fn log_request(
    State(config): State<Arc<Config>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let version = request.version();
    let identifier = requested_identifier(uri.path(), &config.route_prefix);
    let started = Instant::now();
    let span = tracing::info_span!("request", %method, %uri, ?version);
    let response = next.run(request).instrument(span.clone());
    // Errors reported to Honeybadger while handling this request carry its method and path.
    #[cfg(feature = "honeybadger")]
    let response = honeybadger::scope(async {
        honeybadger::context([
            ("method", serde_json::json!(method.as_str())),
            ("path", serde_json::json!(uri.path())),
        ]);
        if let Some(identifier) = &identifier {
            honeybadger::context([("identifier", serde_json::json!(identifier))]);
        }
        response.await
    });
    let response = response.await;
    let status = response.status().as_u16();
    let latency = started.elapsed();
    if let Some(identifier) = identifier {
        info!(
            parent: &span,
            %method,
            %uri,
            %identifier,
            status,
            ?latency,
            "finished processing request"
        );
    } else {
        info!(parent: &span, %method, %uri, status, ?latency, "finished processing request");
    }
    response
}

#[cfg(test)]
mod tests {
    use axum::http::{StatusCode, header};
    use tokio::sync::Semaphore;

    use super::{overloaded_response, try_permit};

    #[test]
    fn admission_fails_immediately_when_capacity_is_used() {
        let semaphore = Semaphore::new(1);
        let permit = try_permit(&semaphore).expect("first request is admitted");
        assert!(try_permit(&semaphore).is_none());
        drop(permit);
        assert!(try_permit(&semaphore).is_some());
    }

    #[test]
    fn overload_response_tells_clients_to_retry() {
        let response = overloaded_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
    }
}
