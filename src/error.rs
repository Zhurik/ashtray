use std::time::Duration;

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::reason::Reason;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The request was understood but the data in it is not acceptable.
    #[error("{}", .0.message())]
    Invalid(Reason),
    /// The request body or query string was not what this endpoint expects.
    #[error("{}", .0.message())]
    BadRequest(Reason),
    /// The client spent its budget for this endpoint.
    #[error("{}", self.reason().message())]
    RateLimited { retry_after: Duration },
    /// The handler did not answer within the request timeout.
    #[error("{}", Reason::Timeout.message())]
    Timeout,
    /// The handler panicked. The panic itself is in the log.
    #[error("{}", Reason::Internal.message())]
    Panic,
    /// A bare status from somewhere that had no body to go with it — the
    /// router's own `404` and `405`, or a layer that refused the request.
    #[error("{}", self.reason().message())]
    Status(StatusCode),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    /// The class of failure, such as `invalid_entry` or `rate_limited`.
    error: &'a str,
    /// The specific cause, for a client that maps it to its own wording.
    reason: Reason,
    /// English prose for a human reading this by hand.
    message: String,
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            // A missing content type is the one body problem that is not a
            // bad request but an unsupported one.
            Self::BadRequest(Reason::UnsupportedContentType) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Status(status) => *status,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Timeout => StatusCode::SERVICE_UNAVAILABLE,
            Self::Panic | Self::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The machine-readable class of the failure. Stable: clients branch on it.
    fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_entry",
            Self::BadRequest(_) | Self::Status(_) => status_code(self.status()),
            Self::RateLimited { .. } => "rate_limited",
            Self::Timeout => "timeout",
            Self::Panic | Self::Database(_) => "internal_error",
        }
    }

    pub fn reason(&self) -> Reason {
        match self {
            Self::Invalid(reason) | Self::BadRequest(reason) => *reason,
            Self::RateLimited { retry_after } => Reason::RateLimited {
                retry_after_secs: retry_after_secs(*retry_after),
            },
            Self::Timeout => Reason::Timeout,
            Self::Panic | Self::Database(_) => Reason::Internal,
            Self::Status(status) => status_reason(*status),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Self::Database(cause) = &self {
            // The cause can carry table and column names, so it goes to the
            // log and never to the client.
            tracing::error!(%cause, "request failed on the database");
        }

        let reason = self.reason();
        let body = ErrorBody {
            error: self.code(),
            reason,
            message: reason.message(),
        };
        let mut response = (self.status(), Json(body)).into_response();

        if let Self::RateLimited { retry_after } = self {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from(retry_after_secs(retry_after)),
            );
        }

        response
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        match rejection {
            JsonRejection::MissingJsonContentType(_) => {
                Self::BadRequest(Reason::UnsupportedContentType)
            }
            JsonRejection::JsonSyntaxError(_) | JsonRejection::JsonDataError(_) => {
                Self::BadRequest(Reason::MalformedJson)
            }
            // The body limit trips inside the JSON extractor; its own message
            // talks about buffers, which means nothing to a visitor.
            rejection if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                Self::Status(StatusCode::PAYLOAD_TOO_LARGE)
            }
            // The rejection is non-exhaustive; anything else means the body
            // never arrived in a readable state.
            _ => Self::BadRequest(Reason::UnreadableBody),
        }
    }
}

impl From<QueryRejection> for ApiError {
    fn from(_: QueryRejection) -> Self {
        Self::BadRequest(Reason::MalformedQuery)
    }
}

/// Whole seconds, rounded up, and never zero: `Retry-After: 0` reads as
/// "hammer away".
fn retry_after_secs(wait: Duration) -> u64 {
    let secs = wait.as_nanos().div_ceil(1_000_000_000).max(1);
    u64::try_from(secs).unwrap_or(u64::MAX)
}

fn status_code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
        StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
        status if status.is_server_error() => "internal_error",
        _ => "bad_request",
    }
}

fn status_reason(status: StatusCode) -> Reason {
    match status {
        StatusCode::UNAUTHORIZED => Reason::Unauthorized,
        StatusCode::NOT_FOUND => Reason::NotFound,
        StatusCode::METHOD_NOT_ALLOWED => Reason::MethodNotAllowed,
        StatusCode::PAYLOAD_TOO_LARGE => Reason::BodyTooLarge,
        status if status.is_server_error() => Reason::Internal,
        other => Reason::Refused(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_rounds_up_and_is_never_zero() {
        assert_eq!(retry_after_secs(Duration::ZERO), 1);
        assert_eq!(retry_after_secs(Duration::from_millis(1)), 1);
        assert_eq!(retry_after_secs(Duration::from_secs(6)), 6);
        assert_eq!(retry_after_secs(Duration::from_millis(6_001)), 7);
    }
}
