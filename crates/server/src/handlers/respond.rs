//! The responses every handler returns: the one JSON error shape, its
//! variants, and conversion calls that survive a panic.

use any_converter_core::convert::{Format, convert_request, convert_response};
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use log::error;

use crate::auth::AuthError;

pub(crate) fn api_error_response(
    status: StatusCode,
    error_type: &str,
    message: impl std::fmt::Display,
) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": {
                "message": message.to_string(),
                "type": error_type,
                "code": error_type,
            }
        })),
    )
        .into_response()
}

/// The data endpoints' error envelope: `{"error": "<message>", "code":
/// "<stable-code>"}`, the shape the Web UI's data client reads. The `code` is
/// a stable machine-readable tag ("storage_error", "invalid_limit", ...), so
/// callers can branch without parsing the message.
pub(crate) fn data_error_response(
    status: StatusCode,
    code: &str,
    message: impl std::fmt::Display,
) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": message.to_string(),
            "code": code,
        })),
    )
        .into_response()
}

pub(crate) fn auth_error_response(err: AuthError) -> Response {
    api_error_response(
        match err {
            AuthError::Missing | AuthError::Invalid => StatusCode::UNAUTHORIZED,
        },
        "authentication_error",
        err,
    )
}

pub(crate) fn upstream_error_response(err: &str) -> Response {
    error!("upstream error: {err}");
    api_error_response(StatusCode::BAD_GATEWAY, "upstream_error", err)
}

pub(crate) fn conversion_error_response(err: HandlerError) -> Response {
    api_error_response(StatusCode::INTERNAL_SERVER_ERROR, "conversion_error", err)
}

#[derive(Debug)]
pub(crate) enum HandlerError {
    Conversion(any_converter_core::error::ConvertError),
    Envelope(String),
    Panic,
}

impl std::fmt::Display for HandlerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandlerError::Conversion(e) => write!(f, "{e}"),
            HandlerError::Envelope(e) => write!(f, "{e}"),
            HandlerError::Panic => write!(f, "conversion not implemented"),
        }
    }
}

pub(crate) fn safe_convert_request(
    input: &[u8],
    from: Format,
    to: Format,
) -> Result<Vec<u8>, HandlerError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        convert_request(input, from, to)
    })) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(e)) => Err(HandlerError::Conversion(e)),
        Err(_) => Err(HandlerError::Panic),
    }
}

pub(crate) fn safe_convert_response(
    input: &[u8],
    from: Format,
    to: Format,
) -> Result<Vec<u8>, HandlerError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        convert_response(input, from, to)
    })) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(e)) => Err(HandlerError::Conversion(e)),
        Err(_) => Err(HandlerError::Panic),
    }
}
