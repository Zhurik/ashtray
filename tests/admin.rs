//! The admin routes, which accept and delete entries for whoever holds the
//! key in `ASHTRAY_ADMIN_KEY` and do not exist for anyone else.

use std::net::SocketAddr;
use std::time::Duration;

use ashtray::config::{HttpConfig, Rate, RateLimitConfig, Secret};
use ashtray::models::EntryLimits;
use ashtray::{AppState, db, router};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

const KEY: &str = "00000000-0000-4000-8000-00000000abcd";

async fn pool() -> SqlitePool {
    db::connect("sqlite::memory:", 1)
        .await
        .expect("in-memory database")
}

fn unlimited() -> HttpConfig {
    HttpConfig {
        rate_limit: RateLimitConfig::disabled(),
        ..HttpConfig::default()
    }
}

/// The app with the waitlist on and the admin key configured.
fn admin_app(pool: SqlitePool) -> Router {
    let state = AppState::new(pool, EntryLimits::default())
        .with_waitlist(true)
        .with_admin_key(Secret::new(KEY));
    router(state, &unlimited())
}

/// A request from one fixed visitor, with `key` in `x-api-key` if given.
fn request(method: &str, uri: &str, key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(key) = key {
        builder = builder.header("x-api-key", key);
    }
    let peer: SocketAddr = "198.51.100.7:50000".parse().expect("peer address");
    builder
        .extensions_mut()
        .expect("fresh builder")
        .insert(ConnectInfo(peer));
    builder.body(Body::empty()).expect("request")
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn post_entry(app: &Router, body: &str) -> i64 {
    let request = Request::builder()
        .method("POST")
        .uri("/api/entries")
        .header("content-type", "application/json")
        .body(Body::from(json!({ "body": body }).to_string()))
        .expect("request");
    let (status, created) = send(app, request).await;
    assert_eq!(status, StatusCode::CREATED);
    created["id"].as_i64().expect("id")
}

async fn public_total(app: &Router) -> Value {
    let (status, page) = send(app, request("GET", "/api/entries", None)).await;
    assert_eq!(status, StatusCode::OK);
    page["total"].clone()
}

async fn stored(pool: &SqlitePool) -> Vec<(i64, bool)> {
    sqlx::query_as("SELECT id, waitlisted FROM entries ORDER BY id")
        .fetch_all(pool)
        .await
        .expect("entries")
}

/// What a path nobody routed answers, for comparison.
async fn unrouted(app: &Router) -> (StatusCode, Value) {
    send(app, request("GET", "/api/does-not-exist", None)).await
}

#[tokio::test]
async fn accepting_lets_a_waitlisted_entry_into_the_list() {
    let app = admin_app(pool().await);
    let id = post_entry(&app, "Let me in.").await;
    assert_eq!(public_total(&app).await, 0);

    let (status, entry) = send(
        &app,
        request("PUT", &format!("/api/entries/{id}/accept"), Some(KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(entry["id"], id);
    assert_eq!(entry["body"], "Let me in.");
    assert_eq!(entry["waitlisted"], false);
    assert_eq!(public_total(&app).await, 1);
}

#[tokio::test]
async fn accepting_twice_is_fine() {
    let app = admin_app(pool().await);
    let id = post_entry(&app, "Twice.").await;
    let uri = format!("/api/entries/{id}/accept");

    send(&app, request("PUT", &uri, Some(KEY))).await;
    let (status, entry) = send(&app, request("PUT", &uri, Some(KEY))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(entry["waitlisted"], false);
}

#[tokio::test]
async fn deleting_removes_the_entry() {
    let pool = pool().await;
    let app = admin_app(pool.clone());
    let gone = post_entry(&app, "Spam.").await;
    let kept = post_entry(&app, "Fine.").await;

    let (status, _) = send(
        &app,
        request("DELETE", &format!("/api/entries/{gone}"), Some(KEY)),
    )
    .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(stored(&pool).await, [(kept, true)]);
}

#[tokio::test]
async fn a_missing_entry_is_not_found_even_with_the_key() {
    let app = admin_app(pool().await);

    let (accept, _) = send(&app, request("PUT", "/api/entries/999/accept", Some(KEY))).await;
    let (delete, _) = send(&app, request("DELETE", "/api/entries/999", Some(KEY))).await;

    assert_eq!(accept, StatusCode::NOT_FOUND);
    assert_eq!(delete, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn without_the_key_the_routes_do_not_exist() {
    let pool = pool().await;
    let app = admin_app(pool.clone());
    let id = post_entry(&app, "Still waiting.").await;
    let expected = unrouted(&app).await;
    assert_eq!(expected.0, StatusCode::NOT_FOUND);

    let upper = KEY.to_uppercase();
    let padded = format!("{KEY} ");
    let keys = [
        None,
        Some("hunter2"),
        Some(upper.as_str()),
        Some(padded.as_str()),
    ];
    let routes = [
        ("PUT", format!("/api/entries/{id}/accept")),
        ("POST", format!("/api/entries/{id}/accept")),
        ("DELETE", format!("/api/entries/{id}")),
        ("GET", format!("/api/entries/{id}")),
        ("DELETE", "/api/entries/not-a-number".to_owned()),
    ];

    for key in keys {
        for (method, uri) in &routes {
            assert_eq!(
                send(&app, request(method, uri, key)).await,
                expected,
                "{method} {uri} with key {key:?}"
            );
        }
    }
    assert_eq!(stored(&pool).await, [(id, true)]);
}

#[tokio::test]
async fn with_no_key_configured_the_routes_do_not_exist() {
    let pool = pool().await;
    let state = AppState::new(pool.clone(), EntryLimits::default()).with_waitlist(true);
    let app = router(state, &unlimited());
    let id = post_entry(&app, "Nobody can let me in.").await;
    let expected = unrouted(&app).await;

    for (method, uri) in [
        ("PUT", format!("/api/entries/{id}/accept")),
        ("DELETE", format!("/api/entries/{id}")),
    ] {
        assert_eq!(send(&app, request(method, &uri, Some(KEY))).await, expected);
    }
    assert_eq!(stored(&pool).await, [(id, true)]);
}

/// The admin app with a tight budget for both reading and writing.
fn limited_admin_app(pool: SqlitePool) -> Router {
    let state = AppState::new(pool, EntryLimits::default()).with_admin_key(Secret::new(KEY));
    let http = HttpConfig {
        rate_limit: RateLimitConfig {
            post: Some(Rate::new(1, Duration::from_secs(3600))),
            get: Some(Rate::new(1, Duration::from_secs(3600))),
        },
        ..HttpConfig::default()
    };
    router(state, &http)
}

#[tokio::test]
async fn without_the_key_the_routes_are_never_rate_limited() {
    let app = limited_admin_app(pool().await);
    send(&app, request("GET", "/api/entries", None)).await;
    let (spent, _) = send(&app, request("GET", "/api/entries", None)).await;
    assert_eq!(spent, StatusCode::TOO_MANY_REQUESTS);

    for (method, uri) in [("DELETE", "/api/entries/1"), ("GET", "/api/entries/1")] {
        let (status, _) = send(&app, request(method, uri, None)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
}

#[tokio::test]
async fn configuring_the_key_does_not_rate_limit_unrouted_paths() {
    let app = limited_admin_app(pool().await);

    for _ in 0..3 {
        for method in ["POST", "GET"] {
            let (status, _) = send(&app, request(method, "/wp-login.php", None)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
        }
    }
}

#[tokio::test]
async fn the_key_spends_the_reading_budget() {
    let app = limited_admin_app(pool().await);

    let (first, _) = send(&app, request("DELETE", "/api/entries/1", Some(KEY))).await;
    let (second, _) = send(&app, request("DELETE", "/api/entries/1", Some(KEY))).await;

    assert_eq!(first, StatusCode::NOT_FOUND);
    assert_eq!(second, StatusCode::TOO_MANY_REQUESTS);
}

/// Status, headers and body of a reply, minus the request id that differs on
/// every reply anyway and the length that only the listener fills in.
async fn reply(
    app: &Router,
    request: Request<Body>,
) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let mut headers: Vec<(String, String)> = response
        .headers()
        .iter()
        // hyper sets the length on the wire for both alike.
        .filter(|(name, _)| *name != "x-request-id" && *name != "content-length")
        .map(|(name, value)| (name.to_string(), value.to_str().unwrap_or("").to_owned()))
        .collect();
    headers.sort();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, headers, body.to_vec())
}

#[tokio::test]
async fn without_the_key_no_header_gives_the_routes_away() {
    let app = admin_app(pool().await);
    let expected = reply(&app, request("GET", "/api/does-not-exist", None)).await;

    for method in ["GET", "HEAD", "PUT", "POST", "PATCH", "DELETE", "OPTIONS"] {
        for uri in ["/api/entries/1", "/api/entries/1/accept"] {
            let mut got = reply(&app, request(method, uri, None)).await;
            let mut expected = expected.clone();
            // A HEAD answer loses its body on the wire either way; in process
            // only the routed one has already lost it.
            if method == "HEAD" {
                got.2.clear();
                expected.2.clear();
            }
            assert_eq!(got, expected, "{method} {uri}");
        }
    }
}

#[tokio::test]
async fn with_the_key_a_wrong_method_is_not_allowed() {
    let app = admin_app(pool().await);

    for (method, uri, allow) in [
        ("POST", "/api/entries/1/accept", "PUT"),
        ("GET", "/api/entries/1", "DELETE"),
    ] {
        let (status, headers, _) = reply(&app, request(method, uri, Some(KEY))).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
        assert!(
            headers.contains(&("allow".to_owned(), allow.to_owned())),
            "{method} {uri}"
        );
    }
}

#[tokio::test]
async fn the_admin_list_is_never_cached() {
    let app = admin_app(pool().await);

    let (_, admin, _) = reply(&app, request("GET", "/api/entries", Some(KEY))).await;
    let (_, public, _) = reply(&app, request("GET", "/api/entries", None)).await;

    let get = |headers: &[(String, String)], name: &str| {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(
        get(&admin, "cache-control").as_deref(),
        Some("private, no-store")
    );
    assert_eq!(get(&admin, "vary").as_deref(), Some("x-api-key"));
    assert_eq!(get(&public, "vary").as_deref(), Some("x-api-key"));
}

#[tokio::test]
async fn the_key_shows_every_entry_with_its_status() {
    let app = admin_app(pool().await);
    let older = post_entry(&app, "Accepted.").await;
    let newer = post_entry(&app, "Waiting.").await;
    send(
        &app,
        request("PUT", &format!("/api/entries/{older}/accept"), Some(KEY)),
    )
    .await;

    let (status, page) = send(&app, request("GET", "/api/entries", Some(KEY))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 2);
    assert_eq!(page["total_pages"], 1);
    assert_eq!(page["items"][0]["id"], newer);
    assert_eq!(page["items"][0]["waitlisted"], true);
    assert_eq!(page["items"][1]["id"], older);
    assert_eq!(page["items"][1]["waitlisted"], false);
}

#[tokio::test]
async fn a_wrong_key_gets_the_public_list() {
    let app = admin_app(pool().await);
    let shown = post_entry(&app, "Accepted.").await;
    post_entry(&app, "Waiting.").await;
    send(
        &app,
        request("PUT", &format!("/api/entries/{shown}/accept"), Some(KEY)),
    )
    .await;

    let (status, page) = send(&app, request("GET", "/api/entries", Some("hunter2"))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["id"], shown);
}

#[tokio::test]
async fn without_admin_configured_the_key_is_ignored_on_the_list() {
    let state = AppState::new(pool().await, EntryLimits::default()).with_waitlist(true);
    let app = router(state, &unlimited());
    post_entry(&app, "Waiting.").await;

    let (status, page) = send(&app, request("GET", "/api/entries", Some(KEY))).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 0);
}
