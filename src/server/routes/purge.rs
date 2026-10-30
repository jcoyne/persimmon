use std::sync::Arc;

use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tracing::warn;

use crate::server::{
    AppState,
    response::{error_response, text_response},
};
use crate::{config::Config, iiif, metrics::Metrics};

#[derive(Deserialize)]
pub(crate) struct PurgeInput {
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

pub(crate) async fn purge(
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
    if !iiif::valid_identifier(&input.identifier) {
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
