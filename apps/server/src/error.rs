//! Mapping domain errors to HTTP responses.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use ticket_api::ErrorResponse;
use ticket_core::Error;
use tracing::error;

/// Error type returned by handlers.
#[derive(Debug)]
pub(crate) enum ApiError {
    /// A domain or adapter error.
    Core(Error),
    /// Missing or invalid credentials.
    Unauthorized,
}

impl From<Error> for ApiError {
    fn from(value: Error) -> Self {
        Self::Core(value)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "missing or invalid credentials".to_owned(),
            ),
            Self::Core(err) => {
                let (status, code) = match &err {
                    Error::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
                    Error::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request"),
                    Error::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
                    Error::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
                    Error::Upstream { .. } => (StatusCode::BAD_GATEWAY, "upstream_error"),
                    Error::Storage(_) | Error::Config(_) => {
                        (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
                    }
                };
                // Server errors are logged in full but never leak internals to callers.
                let message = if status.is_server_error() {
                    error!(error = %err, "request failed");
                    status
                        .canonical_reason()
                        .unwrap_or("internal error")
                        .to_owned()
                } else {
                    err.to_string()
                };
                (status, code, message)
            }
        };
        let body = ErrorResponse {
            code: code.to_owned(),
            message,
        };
        (status, Json(body)).into_response()
    }
}
