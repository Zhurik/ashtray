use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::reason::Reason;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The request was understood but the data in it is not acceptable.
    #[error("{}", .0.message())]
    Invalid(Reason),
    /// The request body was not the JSON this endpoint expects.
    #[error("{}", .0.message())]
    BadRequest(Reason),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Serialize)]
struct ErrorBody {
    /// The class of failure: `invalid_entry`, `bad_request`, `internal_error`.
    error: &'static str,
    /// The specific cause, for a client that maps it to its own wording.
    reason: Reason,
    /// English prose for a human reading this by hand.
    message: String,
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, Reason) {
        match self {
            Self::Invalid(reason) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_entry", *reason),
            Self::BadRequest(reason) => {
                // A missing content type is the one body problem that is not a
                // bad request but an unsupported one.
                let status = if *reason == Reason::UnsupportedContentType {
                    StatusCode::UNSUPPORTED_MEDIA_TYPE
                } else {
                    StatusCode::BAD_REQUEST
                };
                (status, "bad_request", *reason)
            }
            Self::Database(cause) => {
                // The cause can carry table and column names, so it goes to the
                // log and never to the client.
                tracing::error!(%cause, "request failed on the database");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    Reason::Internal,
                )
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, reason) = self.parts();

        (
            status,
            Json(ErrorBody {
                error,
                reason,
                message: reason.message(),
            }),
        )
            .into_response()
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        Self::BadRequest(match rejection {
            JsonRejection::MissingJsonContentType(_) => Reason::UnsupportedContentType,
            JsonRejection::JsonSyntaxError(_) | JsonRejection::JsonDataError(_) => {
                Reason::MalformedJson
            }
            // The rejection is non-exhaustive; anything else means the body
            // never arrived in a readable state.
            _ => Reason::UnreadableBody,
        })
    }
}
