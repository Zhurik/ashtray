use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The request was understood but the data in it is not acceptable.
    #[error("{0}")]
    Invalid(String),
    /// The request body was not the JSON this endpoint expects.
    #[error("{0}")]
    BadRequest(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error) = match &self {
            Self::Invalid(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_entry"),
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::Database(cause) => {
                // The cause can carry table and column names, so it goes to the
                // log and never to the client.
                tracing::error!(%cause, "request failed on the database");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
            }
        };

        let message = match &self {
            Self::Database(_) => "The ashtray could not be reached. Try again.".to_owned(),
            other => other.to_string(),
        };

        (status, Json(ErrorBody { error, message })).into_response()
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        Self::BadRequest(rejection.body_text())
    }
}
