//! The waitlist, which holds new entries back from the list, and the ntfy
//! notifications that announce them.

use std::time::Duration;

use ashtray::config::{HttpConfig, NtfyConfig, RateLimitConfig, Secret};
use ashtray::models::EntryLimits;
use ashtray::notify::Notifier;
use ashtray::{AppState, db, router};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::post;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tower::ServiceExt;

async fn pool() -> SqlitePool {
    db::connect("sqlite::memory:", 1)
        .await
        .expect("in-memory database")
}

fn app(state: AppState) -> Router {
    let http = HttpConfig {
        rate_limit: RateLimitConfig::disabled(),
        ..HttpConfig::default()
    };
    router(state, &http)
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

async fn post_entry(app: &Router, payload: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/api/entries")
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .expect("request");
    send(app, request).await
}

async fn list(app: &Router) -> Value {
    let request = Request::builder()
        .uri("/api/entries")
        .body(Body::empty())
        .expect("request");
    let (status, page) = send(app, request).await;
    assert_eq!(status, StatusCode::OK);
    page
}

#[tokio::test]
async fn without_the_waitlist_an_entry_is_shown_at_once() {
    let app = app(AppState::new(pool().await, EntryLimits::default()));

    let (status, created) = post_entry(&app, json!({"body": "Straight in."})).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["waitlisted"], false);

    let page = list(&app).await;
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["body"], "Straight in.");
}

#[tokio::test]
async fn a_waitlisted_entry_stays_out_of_the_list_until_let_through() {
    let pool = pool().await;
    let app = app(AppState::new(pool.clone(), EntryLimits::default()).with_waitlist(true));

    let (status, created) = post_entry(&app, json!({"body": "Let me in."})).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["waitlisted"], true);

    let page = list(&app).await;
    assert_eq!(page["total"], 0);
    assert_eq!(page["total_pages"], 0);
    assert_eq!(page["items"], json!([]));

    sqlx::query("UPDATE entries SET waitlisted = 0 WHERE id = ?")
        .bind(created["id"].as_i64())
        .execute(&pool)
        .await
        .expect("let the entry through");

    let page = list(&app).await;
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["body"], "Let me in.");
    assert_eq!(page["items"][0]["waitlisted"], false);
}

/// One request as the fake ntfy server saw it.
#[derive(Debug)]
struct Published {
    query: String,
    authorization: Option<String>,
    body: String,
}

/// A fake ntfy server that answers `status` and hands every request it gets
/// to the returned channel. Answers the topic URL to publish to.
async fn fake_ntfy(status: StatusCode) -> (url::Url, mpsc::UnboundedReceiver<Published>) {
    let (sender, received) = mpsc::unbounded_channel();
    let app = Router::new()
        .route(
            "/guestbook",
            post(
                move |State(sender): State<mpsc::UnboundedSender<Published>>,
                      RawQuery(query): RawQuery,
                      headers: HeaderMap,
                      body: Bytes| async move {
                    let _ = sender.send(Published {
                        query: query.unwrap_or_default(),
                        authorization: headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_owned),
                        body: String::from_utf8_lossy(&body).into_owned(),
                    });
                    status
                },
            ),
        )
        .with_state(sender);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("fake ntfy");
    });

    let url = format!("http://{addr}/guestbook").parse().expect("url");
    (url, received)
}

fn notifier(url: url::Url, token: Option<&str>) -> Notifier {
    Notifier::new(&NtfyConfig {
        url: Some(url),
        token: token.map(Secret::new),
    })
    .expect("client")
    .expect("a topic is configured")
}

async fn next(received: &mut mpsc::UnboundedReceiver<Published>) -> Published {
    tokio::time::timeout(Duration::from_secs(5), received.recv())
        .await
        .expect("ntfy is called")
        .expect("fake ntfy still running")
}

#[tokio::test]
async fn no_topic_means_no_notifier() {
    assert!(
        Notifier::new(&NtfyConfig::default())
            .expect("client")
            .is_none()
    );
}

#[tokio::test]
async fn a_new_entry_is_announced_on_ntfy_with_the_token() {
    let (url, mut received) = fake_ntfy(StatusCode::OK).await;
    let state = AppState::new(pool().await, EntryLimits::default())
        .with_waitlist(true)
        .with_notifier(notifier(url, Some("tk_secret")));
    let app = app(state);

    let (status, _) = post_entry(
        &app,
        json!({"author": "Марина", "url": "https://marina.dev", "body": "Привет!"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let published = next(&mut received).await;
    assert_eq!(published.authorization.as_deref(), Some("Bearer tk_secret"));
    let query: Vec<(String, String)> = url::form_urlencoded::parse(published.query.as_bytes())
        .into_owned()
        .collect();
    assert!(query.contains(&(
        "title".to_owned(),
        "New guestbook entry from Марина, waiting for approval".to_owned()
    )));
    assert!(published.body.starts_with("Привет!\n\nhttps://marina.dev"));
}

#[tokio::test]
async fn ntfy_failing_does_not_fail_the_write() {
    let (url, mut received) = fake_ntfy(StatusCode::INTERNAL_SERVER_ERROR).await;
    let state =
        AppState::new(pool().await, EntryLimits::default()).with_notifier(notifier(url, None));
    let app = app(state);

    let (status, _) = post_entry(&app, json!({"body": "Still here."})).await;
    assert_eq!(status, StatusCode::CREATED);

    let published = next(&mut received).await;
    assert_eq!(published.authorization, None);
    assert_eq!(list(&app).await["total"], 1);
}
