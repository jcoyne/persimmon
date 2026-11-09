use std::path::Path;

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use tracing::warn;

use crate::{
    jp2, jxl,
    server::{
        AppState,
        response::{error_response, text_response},
    },
    storage::{LocalCacheFull, SourceLease, SourceNotFound, SourceTooLarge},
};

use super::Step;

/// Reads the purge-controlled generation token that namespaces cached responses.
pub(super) async fn current_cache_generation(state: &AppState, identifier: &str) -> Step<String> {
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
        .map_err(fetch_error_response)
}

fn fetch_error_response(error: anyhow::Error) -> Response {
    if error.is::<LocalCacheFull>() {
        // Capacity, like request admission: the sources in use free their space
        // when their requests finish, so ask the client to retry rather than
        // reporting a server fault.
        warn!(error = %error, "rejected source download");
        let mut response = text_response(StatusCode::SERVICE_UNAVAILABLE, &error.to_string());
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return response;
    }
    error_response(fetch_error_status(&error), error)
}

fn fetch_error_status(error: &anyhow::Error) -> StatusCode {
    if error.is::<SourceNotFound>() {
        StatusCode::NOT_FOUND
    } else if error.is::<SourceTooLarge>() {
        // Not 503: the object will be just as large on retry. Still a server error, so an
        // operator sees it and can raise PERSIMMON_MAX_SOURCE_BYTES.
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
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

/// How much of a source to read when looking for its header. Image headers sit at
/// the start of the file, ahead of any large metadata or codestream boxes.
const HEADER_PREFIX_BYTES: u64 = 64 * 1024;

/// Reads the source's width, height and component count from a ranged read of its
/// first bytes, downloading the whole source only when the header is not there.
pub(super) async fn dimensions(
    state: &AppState,
    identifier: &str,
    generation: &str,
) -> Step<(u32, u32, u16)> {
    let (prefix, total) = state
        .storage
        .source_prefix(identifier, HEADER_PREFIX_BYTES)
        .await
        .map_err(|e| error_response(fetch_error_status(&e), e))?;
    let parsed = tokio::task::spawn_blocking(move || {
        if jxl::has_signature(&prefix) {
            jxl::metadata_from_prefix(&prefix)
        } else {
            jp2::metadata_from_prefix(&prefix, total)
        }
    })
    .await;
    match parsed {
        Ok(Ok(Some(dimensions))) => Ok(dimensions),
        Ok(Ok(None)) => {
            let source = fetch(state, identifier, generation).await?;
            Ok(probe(&source.path).await?.0)
        }
        Ok(Err(e)) => Err(error_response(StatusCode::INTERNAL_SERVER_ERROR, e)),
        Err(e) => Err(error_response(StatusCode::INTERNAL_SERVER_ERROR, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_source_is_not_reported_as_temporarily_unavailable() {
        let error = anyhow::Error::new(SourceTooLarge {
            size: 300_000_000,
            limit: 250_000_000,
        });
        assert_eq!(
            fetch_error_status(&error),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            error.to_string(),
            "source exceeds configured byte limit (300000000 > 250000000 bytes)"
        );
    }

    #[test]
    fn missing_source_is_not_found_and_other_failures_are_unavailable() {
        assert_eq!(
            fetch_error_status(&SourceNotFound.into()),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            fetch_error_status(&anyhow::anyhow!("connection reset")),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
