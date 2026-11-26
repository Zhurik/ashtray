//! The admin routes: accepting and deleting entries.
//!
//! They answer only requests that carry the key from `ASHTRAY_ADMIN_KEY` in
//! `x-api-key`. Everyone else gets the same `404` as a path nobody routed,
//! whatever the method, so the routes cannot be told apart from nothing.

use axum::Json;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::error::ApiError;
use crate::models::Entry;

pub const API_KEY: HeaderName = HeaderName::from_static("x-api-key");

/// Whether `headers` carry the admin key. Never true when none is configured.
pub fn is_admin(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(key) = &state.admin_key else {
        return false;
    };
    headers
        .get(API_KEY)
        .is_some_and(|presented| constant_time_eq(presented.as_bytes(), key.expose().as_bytes()))
}

/// Lets the request through to the admin routes only with the key.
pub async fn require_key(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if is_admin(&state, request.headers()) {
        next.run(request).await
    } else {
        ApiError::Status(StatusCode::NOT_FOUND).into_response()
    }
}

/// PUT /api/entries/{id}/accept — lets an entry into the list. Accepting one
/// that is already shown changes nothing and is not an error.
pub async fn accept(
    State(state): State<AppState>,
    method: Method,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    if method != Method::PUT {
        return Ok(method_not_allowed("PUT"));
    }
    sqlx::query_as::<_, Entry>(
        "UPDATE entries SET waitlisted = 0 WHERE id = ?
         RETURNING id, author, url, body, created_at, waitlisted",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .map(|entry| Json(entry).into_response())
    .ok_or(ApiError::Status(StatusCode::NOT_FOUND))
}

/// DELETE /api/entries/{id} — removes an entry for good.
pub async fn delete(
    State(state): State<AppState>,
    method: Method,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    if method != Method::DELETE {
        return Ok(method_not_allowed("DELETE"));
    }
    let deleted = sqlx::query("DELETE FROM entries WHERE id = ?")
        .bind(id)
        .execute(&state.pool)
        .await?
        .rows_affected();

    if deleted == 0 {
        Err(ApiError::Status(StatusCode::NOT_FOUND))
    } else {
        Ok(StatusCode::NO_CONTENT.into_response())
    }
}

/// What the router would answer for a wrong method, had it been left to it.
fn method_not_allowed(allow: &'static str) -> Response {
    let mut response = ApiError::Status(StatusCode::METHOD_NOT_ALLOWED).into_response();
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static(allow));
    response
}

/// Compares without stopping at the first difference, so the time taken does
/// not tell an attacker how much of the key they already have right.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_compare_whole() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret-and-more"));
    }
}
