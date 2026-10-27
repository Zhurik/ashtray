use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use chrono::{SecondsFormat, Utc};

use crate::AppState;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::models::{Entry, EntryPage, NewEntry, Pagination};

/// POST /api/entries — writes one entry and hands back what was stored.
pub async fn create(
    State(state): State<AppState>,
    ApiJson(payload): ApiJson<NewEntry>,
) -> Result<(StatusCode, Json<Entry>), ApiError> {
    let entry = payload.validate()?;
    // Second precision in UTC: fixed width, so the text column sorts
    // chronologically without any parsing.
    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    let stored = sqlx::query_as::<_, Entry>(
        "INSERT INTO entries (author, url, body, created_at)
         VALUES (?, ?, ?, ?)
         RETURNING id, author, url, body, created_at",
    )
    .bind(&entry.author)
    .bind(&entry.url)
    .bind(&entry.body)
    .bind(&created_at)
    .fetch_one(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(stored)))
}

/// GET /api/entries — one page of entries, newest first.
pub async fn list(
    State(state): State<AppState>,
    Query(pagination): Query<Pagination>,
) -> Result<Json<EntryPage>, ApiError> {
    let per_page = pagination.per_page();
    let page = pagination.page();

    let total = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM entries")
        .fetch_one(&state.pool)
        .await?;

    let items = sqlx::query_as::<_, Entry>(
        "SELECT id, author, url, body, created_at
         FROM entries
         ORDER BY created_at DESC, id DESC
         LIMIT ? OFFSET ?",
    )
    .bind(i64::from(per_page))
    .bind(pagination.offset())
    .fetch_all(&state.pool)
    .await?;

    // `div_ceil` is stable for unsigned integers only, and the count is never negative.
    let total_pages = (total as u64).div_ceil(u64::from(per_page)) as u32;

    Ok(Json(EntryPage {
        items,
        page,
        per_page,
        total,
        total_pages,
    }))
}
