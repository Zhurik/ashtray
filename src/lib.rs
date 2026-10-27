pub mod config;
pub mod db;
pub mod error;
pub mod extract;
pub mod models;
pub mod routes;

use axum::Router;
use axum::routing::{get, post};
use sqlx::SqlitePool;
use tower_http::trace::TraceLayer;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(routes::health::health))
        .route(
            "/api/entries",
            post(routes::entries::create).get(routes::entries::list),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
