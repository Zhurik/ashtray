use ashtray::{AppState, db, router};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// A router over a private in-memory database. The pool is capped at one
/// connection because every connection to `sqlite::memory:` would otherwise
/// get a database of its own.
async fn app() -> Router {
    let pool = db::connect("sqlite::memory:", 1)
        .await
        .expect("in-memory database");
    router(AppState { pool })
}

async fn post_entry(app: &Router, payload: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/api/entries")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .expect("request");

    send(app, request).await
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    send(app, request).await
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn health_reports_the_database_is_up() {
    let app = app().await;

    let (status, body) = get(&app, "/health").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["database"], "up");
}

#[tokio::test]
async fn an_entry_comes_back_in_the_list_it_was_written_to() {
    let app = app().await;

    let (status, created) = post_entry(
        &app,
        json!({"author": "marina", "url": "https://marina.dev", "body": "Hello from the webring."}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["author"], "marina");
    assert_eq!(created["url"], "https://marina.dev");
    assert!(created["id"].as_i64().expect("id") > 0);

    let (status, page) = get(&app, "/api/entries").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["body"], "Hello from the webring.");
}

#[tokio::test]
async fn an_entry_without_a_name_or_a_link_is_anonymous() {
    let app = app().await;

    let (status, created) = post_entry(&app, json!({"body": "No name, no link."})).await;

    assert_eq!(status, StatusCode::CREATED);
    assert!(created["author"].is_null());
    assert!(created["url"].is_null());
}

#[tokio::test]
async fn blank_strings_count_as_absent() {
    let app = app().await;

    let (status, created) = post_entry(
        &app,
        json!({"author": "   ", "url": "", "body": "  spaced  "}),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert!(created["author"].is_null());
    assert!(created["url"].is_null());
    assert_eq!(created["body"], "spaced");
}

#[tokio::test]
async fn the_newest_entry_is_first() {
    let app = app().await;

    for body in ["first", "second", "third"] {
        let (status, _) = post_entry(&app, json!({"body": body})).await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let (_, page) = get(&app, "/api/entries").await;

    let bodies: Vec<&str> = page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["body"].as_str().expect("body"))
        .collect();
    assert_eq!(bodies, ["third", "second", "first"]);
}

#[tokio::test]
async fn pages_split_the_entries_and_never_repeat_one() {
    let app = app().await;

    for index in 0..5 {
        post_entry(&app, json!({"body": format!("entry {index}")})).await;
    }

    let (_, first) = get(&app, "/api/entries?page=1&per_page=2").await;
    assert_eq!(first["total"], 5);
    assert_eq!(first["total_pages"], 3);
    assert_eq!(first["items"].as_array().expect("items").len(), 2);

    let (_, last) = get(&app, "/api/entries?page=3&per_page=2").await;
    assert_eq!(last["items"].as_array().expect("items").len(), 1);

    let (_, past_the_end) = get(&app, "/api/entries?page=9&per_page=2").await;
    assert!(past_the_end["items"].as_array().expect("items").is_empty());
    assert_eq!(past_the_end["total"], 5);
}

#[tokio::test]
async fn an_oversized_page_size_is_clamped_instead_of_rejected() {
    let app = app().await;
    post_entry(&app, json!({"body": "only one"})).await;

    let (status, page) = get(&app, "/api/entries?per_page=5000&page=0").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["per_page"], 100);
    assert_eq!(page["page"], 1);
}

#[tokio::test]
async fn an_empty_message_is_refused() {
    let app = app().await;

    let (status, body) = post_entry(&app, json!({"body": "   "})).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid_entry");
}

#[tokio::test]
async fn a_message_over_the_limit_is_refused() {
    let app = app().await;

    let (status, _) = post_entry(&app, json!({"body": "x".repeat(801)})).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn the_limits_count_characters_not_bytes() {
    let app = app().await;

    let (status, _) = post_entry(&app, json!({"body": "щ".repeat(800)})).await;

    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn a_link_that_is_not_http_is_refused() {
    let app = app().await;

    for url in [
        "javascript:alert(1)",
        "not a url",
        "ftp://files.example.com",
    ] {
        let (status, body) = post_entry(&app, json!({"url": url, "body": "hi"})).await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{url} should be refused"
        );
        assert_eq!(body["error"], "invalid_entry");
    }
}

#[tokio::test]
async fn a_body_that_is_not_json_gets_a_json_error_back() {
    let app = app().await;

    let request = Request::builder()
        .method("POST")
        .uri("/api/entries")
        .header("content-type", "application/json")
        .body(Body::from("{nope"))
        .expect("request");

    let (status, body) = send(&app, request).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "bad_request");
}
