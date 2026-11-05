// Handler steps return the early-exit `Response` as their error. It is large,
// but a request builds at most one, so boxing it would gain nothing.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};

use crate::{
    config::Config,
    iiif::{self, Route},
    metrics::Metrics,
    server::{
        AppState,
        response::{error_response, text_response},
    },
};

mod image;
mod information;
mod source;

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
        Route::Info(identifier) => information::serve(state, &identifier, headers).await,
        Route::Image(request) => image::serve(state, raw_path, &request).await,
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

#[cfg(test)]
mod tests {
    use super::{iiif_path, requested_identifier};

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
}
