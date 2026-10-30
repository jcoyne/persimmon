use std::sync::Arc;

use axum::{
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::server::AppState;

pub(crate) async fn metrics(State(state): State<Arc<AppState>>) -> Response {
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
