//! Small layers that every request goes through on its way to a handler.

use std::any::Any;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::Method;
use axum::http::header::{self, HeaderName};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::RequestId;
use tracing::Span;

use crate::client_ip::ClientIp;
use crate::config::{AllowedOrigins, CorsConfig};
use crate::error::ApiError;

pub const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

const HEALTH_PATH: &str = "/health";

/// Gives up on a handler that has not answered within `limit`.
pub async fn timeout(State(limit): State<Duration>, request: Request, next: Next) -> Response {
    match tokio::time::timeout(limit, next.run(request)).await {
        Ok(response) => response,
        Err(_) => {
            tracing::warn!(timeout = ?limit, "request timed out");
            ApiError::Timeout.into_response()
        }
    }
}

/// Gives every error the JSON body the handlers give theirs.
///
/// The router's own `404` and `405` and anything a layer refuses come back
/// with a status and nothing else. This fills in the body for those — and only
/// those: a response that already has a content type is left alone, and the
/// headers of the original response, such as `Allow`, survive.
pub async fn json_errors(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let status = response.status();

    let is_error = status.is_client_error() || status.is_server_error();
    if !is_error || response.headers().contains_key(header::CONTENT_TYPE) {
        return response;
    }

    let (mut parts, _) = response.into_parts();
    let (replacement, body) = ApiError::Status(status).into_response().into_parts();
    // The old length described the old, empty body.
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.extend(replacement.headers);
    Response::from_parts(parts, body)
}

/// What a panicking handler answers instead of a dropped connection.
pub fn panic_response(panic: Box<dyn Any + Send + 'static>) -> Response {
    let message = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("a panic without a message");
    tracing::error!(panic = message, "handler panicked");

    ApiError::Panic.into_response()
}

/// The CORS policy for `config`. Methods and headers belong to the API rather
/// than to a deployment, so only the origins and the cache time come from the
/// environment.
pub fn cors(config: &CorsConfig) -> CorsLayer {
    let origins = match &config.origins {
        AllowedOrigins::Any => AllowOrigin::any(),
        AllowedOrigins::List(origins) => AllowOrigin::list(origins.iter().cloned()),
    };

    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::HEAD, Method::POST])
        .allow_headers([header::CONTENT_TYPE])
        // A script reading a 429 needs to know when to come back, and one
        // reporting a failure is more useful with the id the log knows it by.
        .expose_headers([header::RETRY_AFTER, REQUEST_ID])
        .max_age(config.max_age)
}

/// The span every request is logged under.
///
/// Health checks get a `DEBUG` span: a monitor polling every few seconds
/// would otherwise bury the visitors in the log. With `ASHTRAY_LOG=info`
/// they are silent, with `debug` they show up like everything else.
pub fn request_span(request: &Request) -> Span {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .unwrap_or("-");
    let client = request
        .extensions()
        .get::<ClientIp>()
        .map(|ip| ip.0.to_string())
        .unwrap_or_default();

    if request.uri().path() == HEALTH_PATH {
        tracing::debug_span!(
            "request",
            method = %request.method(),
            uri = %request.uri(),
            request_id,
            client,
        )
    } else {
        tracing::info_span!(
            "request",
            method = %request.method(),
            uri = %request.uri(),
            request_id,
            client,
        )
    }
}

/// One line per answered request, unless its span is filtered out.
pub fn log_response(response: &Response, latency: Duration, span: &Span) {
    if span.is_disabled() {
        return;
    }
    tracing::info!(
        parent: span,
        status = response.status().as_u16(),
        latency_ms = latency.as_millis(),
        "answered"
    );
}
