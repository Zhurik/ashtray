pub mod client_ip;
pub mod config;
pub mod db;
pub mod error;
pub mod extract;
pub mod healthcheck;
pub mod metrics;
pub mod middleware;
pub mod models;
pub mod rate_limit;
pub mod reason;
pub mod routes;
pub mod server;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::HeaderValue;
use axum::http::header::{self, HeaderName};
use axum::routing::{get, post};
use sqlx::SqlitePool;
use tower::ServiceBuilder;
use tower::util::option_layer;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::config::HttpConfig;
use crate::metrics::Metrics;
use crate::middleware::REQUEST_ID;
use crate::models::EntryLimits;
use crate::rate_limit::RateLimits;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub limits: EntryLimits,
    pub metrics: Arc<Metrics>,
}

impl AppState {
    pub fn new(pool: SqlitePool, limits: EntryLimits) -> Self {
        let metrics = Arc::new(Metrics::new(&pool));
        Self {
            pool,
            limits,
            metrics,
        }
    }
}

/// The whole service as one router.
///
/// A request passes the layers from the top of the stack to the bottom and
/// the response comes back up:
///
/// 1. a request id is assigned (or an incoming `x-request-id` kept) and
///    echoed back;
/// 2. the client address is resolved, through trusted proxies if any;
/// 3. the request is counted and timed for the metrics, and traced under a
///    span carrying both;
/// 4. security headers are set on the way out;
/// 5. CORS answers preflights and stamps the response, errors included;
/// 6. bodiless errors get the JSON shape the handlers use;
/// 7. the handler is given `request_timeout` to answer;
/// 8. a panic becomes a JSON `500` rather than a dropped connection;
/// 9. bodies above `max_body_bytes` are refused;
///
/// and on `/api/entries` alone, the client's rate limit is spent. `/health`
/// is never limited: a monitor polling it is not a visitor.
pub fn router(state: AppState, http: &HttpConfig) -> Router {
    let entries = Router::new()
        .route(
            "/api/entries",
            post(routes::entries::create).get(routes::entries::list),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            RateLimits::new(&http.rate_limit, state.metrics.clone()),
            rate_limit::enforce,
        ));

    let layers = ServiceBuilder::new()
        .layer(SetRequestIdLayer::new(REQUEST_ID, MakeRequestUuid))
        .layer(PropagateRequestIdLayer::new(REQUEST_ID))
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(http.client_ip.clone()),
            client_ip::resolve,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.metrics.clone(),
            metrics::track,
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(middleware::request_span)
                .on_response(middleware::log_response),
        )
        // The API never serves a page, so the strictest values cost nothing:
        // no sniffing a response into HTML, no framing it, no referrer.
        .layer(security_header(header::X_CONTENT_TYPE_OPTIONS, "nosniff"))
        .layer(security_header(header::X_FRAME_OPTIONS, "DENY"))
        .layer(security_header(header::REFERRER_POLICY, "no-referrer"))
        .layer(security_header(
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; frame-ancestors 'none'",
        ))
        .layer(option_layer(http.cors.as_ref().map(middleware::cors)))
        .layer(axum::middleware::from_fn(middleware::json_errors))
        .layer(axum::middleware::from_fn_with_state(
            http.request_timeout,
            middleware::timeout,
        ))
        .layer(CatchPanicLayer::custom(middleware::panic_response))
        .layer(DefaultBodyLimit::max(http.max_body_bytes));

    Router::new()
        .route("/health", get(routes::health::health))
        .merge(entries)
        .layer(layers)
        .with_state(state)
}

fn security_header(name: HeaderName, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::if_not_present(name, HeaderValue::from_static(value))
}
