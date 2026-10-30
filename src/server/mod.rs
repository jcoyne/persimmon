use std::{sync::Arc, time::Instant};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::Request,
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
};
use tokio::sync::{Mutex, Semaphore};
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
    temp_bitmaps: Arc<Semaphore>,
    derivative_locks: Vec<Mutex<()>>,
    health: Mutex<Option<(Instant, HealthReport)>>,
}

impl AppState {
    pub fn new(config: Arc<Config>, storage: Arc<Storage>) -> Self {
        Self {
            decodes: Semaphore::new(config.max_parallel_decodes),
            temp_bitmaps: Arc::new(Semaphore::new(
                (config.max_temp_bitmap_bytes / 1_048_576) as usize,
            )),
            derivative_locks: (0..256).map(|_| Mutex::new(())).collect(),
            health: Mutex::new(None),
            config,
            storage,
        }
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let config = state.config.clone();
    Router::new()
        .route("/healthz", get(routes::health::health))
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
    let response = next.run(request).instrument(span.clone()).await;
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
