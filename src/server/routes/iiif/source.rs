use std::path::Path;

use axum::http::StatusCode;

use crate::{
    jp2, jxl,
    server::{AppState, response::error_response},
    storage::{SourceLease, SourceNotFound},
};

use super::Step;

pub(super) async fn current_generation(state: &AppState, identifier: &str) -> Step<String> {
    state
        .storage
        .generation(identifier)
        .await
        .map_err(|e| error_response(StatusCode::SERVICE_UNAVAILABLE, e))
}

pub(super) async fn fetch(
    state: &AppState,
    identifier: &str,
    generation: &str,
) -> Step<SourceLease> {
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
pub(super) async fn probe(path: &Path) -> Step<((u32, u32, u16), bool)> {
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
