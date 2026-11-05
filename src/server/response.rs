use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use tracing::error;

pub(crate) fn text_response(status: StatusCode, message: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        message.to_owned(),
    )
        .into_response()
}

pub(crate) fn error_response(status: StatusCode, error: impl std::fmt::Display) -> Response {
    if status.is_server_error() {
        error!(status = status.as_u16(), error = %error, "request failed");
        // 501 answers requests for unsupported features, such as upscaling.
        #[cfg(feature = "honeybadger")]
        if status != StatusCode::NOT_IMPLEMENTED {
            honeybadger::notify_notice(
                // The alternate form includes the cause chain; AWS SDK errors alone say little.
                honeybadger::Notice::message("persimmon::ServerError", &format!("{error:#}"))
                    .context([("status", serde_json::json!(status.as_u16()))]),
            );
        }
        text_response(status, status.canonical_reason().unwrap_or("server error"))
    } else {
        text_response(status, &error.to_string())
    }
}
