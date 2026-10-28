use std::time::Duration;

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The request was understood but the data in it is not acceptable.
    #[error("{0}")]
    Invalid(String),
    /// The request body or query string was not what this endpoint expects.
    #[error("{0}")]
    BadRequest(String),
    /// An extractor refused the request with a status of its own, such as
    /// `415` for a body that is not declared as JSON.
    #[error("{message}")]
    Rejected { status: StatusCode, message: String },
    /// The client spent its budget for this endpoint.
    #[error("Too many requests. Try again in {} seconds.", retry_after_secs(*.retry_after))]
    RateLimited { retry_after: Duration },
    /// The handler did not answer within the request timeout.
    #[error("The ashtray took too long to answer. Try again.")]
    Timeout,
    /// The handler panicked. The panic itself is in the log.
    #[error("Something broke inside the ashtray. Try again.")]
    Panic,
    /// A bare status from somewhere that had no body to go with it — the
    /// router's own `404` and `405`, or a layer that refused the request.
    #[error("{}", status_message(*.0))]
    Status(StatusCode),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: String,
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Rejected { status, .. } | Self::Status(status) => *status,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::Timeout => StatusCode::SERVICE_UNAVAILABLE,
            Self::Panic | Self::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The machine-readable half of the body. Stable: clients branch on it.
    fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_entry",
            Self::BadRequest(_) => "bad_request",
            Self::RateLimited { .. } => "rate_limited",
            Self::Timeout => "timeout",
            Self::Panic | Self::Database(_) => "internal_error",
            Self::Rejected { status, .. } | Self::Status(status) => status_code(*status),
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

        let status = self.status();
        let error = self.code();
        let message = match &self {
            Self::Database(_) => "The ashtray could not be reached. Try again.".to_owned(),
            other => other.to_string(),
        };

        let mut response = (status, Json(ErrorBody { error, message })).into_response();

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
        match rejection.status() {
            // The body limit trips inside the JSON extractor; its own message
            // talks about buffers, which means nothing to a visitor.
            StatusCode::PAYLOAD_TOO_LARGE => Self::Status(StatusCode::PAYLOAD_TOO_LARGE),
            StatusCode::UNSUPPORTED_MEDIA_TYPE => Self::Rejected {
                status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                message: rejection.body_text(),
            },
            // Broken syntax and the wrong fields alike: the client sent
            // something this endpoint cannot read.
            _ => Self::BadRequest(rejection.body_text()),
        }
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rejection: QueryRejection) -> Self {
        Self::BadRequest(rejection.body_text())
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
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
        StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
        status if status.is_server_error() => "internal_error",
        _ => "bad_request",
    }
}

fn status_message(status: StatusCode) -> String {
    match status {
        StatusCode::NOT_FOUND => "Nothing lives at this address.".to_owned(),
        StatusCode::METHOD_NOT_ALLOWED => "This address does not take that method.".to_owned(),
        StatusCode::PAYLOAD_TOO_LARGE => "The request is too large.".to_owned(),
        other => format!("{}.", other.canonical_reason().unwrap_or("Request failed")),
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
