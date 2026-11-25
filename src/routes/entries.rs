use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use chrono::{SecondsFormat, Utc};

use crate::AppState;
use crate::admin;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiQuery};
use crate::models::{Entry, EntryPage, NewEntry, Pagination};

/// POST /api/entries — writes one entry and hands back what was stored. With
/// the waitlist on, the entry is stored hidden and says so in `waitlisted`.
pub async fn create(
    State(state): State<AppState>,
    ApiJson(payload): ApiJson<NewEntry>,
) -> Result<(StatusCode, Json<Entry>), ApiError> {
    let entry = payload.validate(&state.limits)?;
    // Second precision in UTC: fixed width, so the text column sorts
    // chronologically without any parsing.
    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    let stored = sqlx::query_as::<_, Entry>(
        "INSERT INTO entries (author, url, body, created_at, waitlisted)
         VALUES (?, ?, ?, ?, ?)
         RETURNING id, author, url, body, created_at, waitlisted",
    )
    .bind(&entry.author)
    .bind(&entry.url)
    .bind(&entry.body)
    .bind(&created_at)
    .bind(state.waitlist)
    .fetch_one(&state.pool)
    .await?;
    state.metrics.entry_created();
    if let Some(notifier) = &state.notifier {
        notifier.entry_created(&stored);
    }

    Ok((StatusCode::CREATED, Json(stored)))
}

const COUNT_SHOWN: &str = "SELECT COUNT(*) FROM entries WHERE waitlisted = 0";
const COUNT_ALL: &str = "SELECT COUNT(*) FROM entries";
const PAGE_SHOWN: &str = "SELECT id, author, url, body, created_at, waitlisted
     FROM entries
     WHERE waitlisted = 0
     ORDER BY created_at DESC, id DESC
     LIMIT ? OFFSET ?";
const PAGE_ALL: &str = "SELECT id, author, url, body, created_at, waitlisted
     FROM entries
     ORDER BY created_at DESC, id DESC
     LIMIT ? OFFSET ?";

/// GET /api/entries — one page of entries, newest first. Waitlisted entries
/// are left out, of the count as well as of the page, unless the request
/// carries the admin key; a wrong key just gets the public list.
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(pagination): ApiQuery<Pagination>,
) -> Result<Json<EntryPage>, ApiError> {
    let per_page = pagination.per_page(&state.limits);
    let page = pagination.page();
    // Fixed strings rather than a bound flag, so the public query keeps
    // matching the partial index on shown entries.
    let (count, select) = if admin::is_admin(&state, &headers) {
        (COUNT_ALL, PAGE_ALL)
    } else {
        (COUNT_SHOWN, PAGE_SHOWN)
    };

    let total = sqlx::query_scalar::<_, i64>(count)
        .fetch_one(&state.pool)
        .await?;

    let items = sqlx::query_as::<_, Entry>(select)
        .bind(i64::from(per_page))
        .bind(pagination.offset(&state.limits))
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
