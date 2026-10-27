use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
pub struct Health {
    status: &'static str,
    database: &'static str,
}

/// Answers whether the process is up *and* whether it can still reach SQLite —
/// a health check that only proves the former is not worth polling.
pub async fn health(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    match sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(Health {
                status: "ok",
                database: "up",
            }),
        ),
        Err(cause) => {
            tracing::error!(%cause, "health check could not reach the database");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(Health {
                    status: "degraded",
                    database: "down",
                }),
            )
        }
    }
}
