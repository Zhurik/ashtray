//! What stands between the handlers and the internet: CORS, rate limits,
//! client addresses, body and time limits, and the shape of every error.

use std::net::SocketAddr;
use std::time::Duration;

use ashtray::config::{
    AllowedOrigins, ClientIpConfig, CorsConfig, HttpConfig, Rate, RateLimitConfig,
};
use ashtray::models::EntryLimits;
use ashtray::{AppState, db, middleware, router};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::routing::get;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use tower_http::catch_panic::CatchPanicLayer;

const SITE: &str = "https://smoking-room.example";
const VISITOR: &str = "198.51.100.7:50000";
const PROXY: &str = "10.0.0.2:40000";

async fn app_with(http: HttpConfig, limits: EntryLimits) -> Router {
    let pool = db::connect("sqlite::memory:", 1)
        .await
        .expect("in-memory database");
    router(AppState::new(pool, limits), &http)
}

async fn app(http: HttpConfig) -> Router {
    app_with(http, EntryLimits::default()).await
}

fn unlimited() -> HttpConfig {
    HttpConfig {
        rate_limit: RateLimitConfig::disabled(),
        ..HttpConfig::default()
    }
}

fn with_cors(origins: AllowedOrigins) -> HttpConfig {
    HttpConfig {
        cors: Some(CorsConfig {
            origins,
            max_age: Duration::from_secs(600),
        }),
        ..unlimited()
    }
}

fn site_only() -> HttpConfig {
    with_cors(AllowedOrigins::List(vec![HeaderValue::from_static(SITE)]))
}

fn limited(post: Option<Rate>, get: Option<Rate>) -> HttpConfig {
    HttpConfig {
        rate_limit: RateLimitConfig { post, get },
        client_ip: ClientIpConfig {
            trusted_proxies: vec!["10.0.0.0/8".parse().unwrap()],
            ..ClientIpConfig::default()
        },
        ..HttpConfig::default()
    }
}

/// A request as the listener hands it over: with the TCP peer attached.
fn request(method: Method, uri: &str, peer: &str) -> axum::http::request::Builder {
    let peer: SocketAddr = peer.parse().expect("peer address");
    let mut builder = Request::builder().method(method).uri(uri);
    builder
        .extensions_mut()
        .expect("fresh builder")
        .insert(ConnectInfo(peer));
    builder
}

fn new_entry(peer: &str) -> Request<Body> {
    request(Method::POST, "/api/entries", peer)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"body": "hi"}).to_string()))
        .expect("request")
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

async fn send(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    Reply {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn preflight(origin: &str) -> Request<Body> {
    request(Method::OPTIONS, "/api/entries", VISITOR)
        .header(header::ORIGIN, origin)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
        .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
        .body(Body::empty())
        .expect("request")
}

// ---------- CORS ----------

#[tokio::test]
async fn cors_is_off_unless_configured() {
    let app = app(unlimited()).await;

    let reply = send(&app, preflight(SITE)).await;

    assert!(
        !reply
            .headers
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );
}

#[tokio::test]
async fn an_allowed_origin_passes_the_preflight() {
    let app = app(site_only()).await;

    let reply = send(&app, preflight(SITE)).await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], SITE);
    let methods = reply.headers[header::ACCESS_CONTROL_ALLOW_METHODS]
        .to_str()
        .unwrap();
    assert!(methods.contains("POST"), "{methods}");
    assert_eq!(reply.headers[header::ACCESS_CONTROL_MAX_AGE], "600");
}

#[tokio::test]
async fn another_origin_gets_no_permission() {
    let app = app(site_only()).await;

    let reply = send(&app, preflight("https://casino.example")).await;

    assert!(
        !reply
            .headers
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );
}

#[tokio::test]
async fn a_star_lets_any_origin_in() {
    let app = app(with_cors(AllowedOrigins::Any)).await;

    let reply = send(&app, preflight("https://anyone.example")).await;

    assert_eq!(reply.headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
}

#[tokio::test]
async fn a_script_can_read_the_retry_after_of_a_refusal() {
    let mut http = site_only();
    http.rate_limit.post = Some(Rate::new(1, Duration::from_secs(60)));
    let app = app(http).await;

    let from_site = || {
        let mut request = new_entry(VISITOR);
        request
            .headers_mut()
            .insert(header::ORIGIN, HeaderValue::from_static(SITE));
        request
    };
    send(&app, from_site()).await;
    let reply = send(&app, from_site()).await;

    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(reply.headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], SITE);
    let exposed = reply.headers[header::ACCESS_CONTROL_EXPOSE_HEADERS]
        .to_str()
        .unwrap();
    assert!(exposed.contains("retry-after"), "{exposed}");
}

// ---------- rate limits ----------

#[tokio::test]
async fn a_client_over_its_budget_is_told_when_to_come_back() {
    let app = app(limited(Some(Rate::new(2, Duration::from_secs(60))), None)).await;

    assert_eq!(
        send(&app, new_entry(VISITOR)).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        send(&app, new_entry(VISITOR)).await.status,
        StatusCode::CREATED
    );
    let reply = send(&app, new_entry(VISITOR)).await;

    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(reply.body["error"], "rate_limited");
    assert_eq!(reply.body["reason"], "rate_limited");
    let retry_after: u64 = reply.headers[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=30).contains(&retry_after), "{retry_after}");
}

#[tokio::test]
async fn reading_and_writing_have_separate_budgets() {
    let one_a_minute = Some(Rate::new(1, Duration::from_secs(60)));
    let app = app(limited(one_a_minute, one_a_minute)).await;
    let list = || {
        request(Method::GET, "/api/entries", VISITOR)
            .body(Body::empty())
            .unwrap()
    };

    assert_eq!(
        send(&app, new_entry(VISITOR)).await.status,
        StatusCode::CREATED
    );
    assert_eq!(send(&app, list()).await.status, StatusCode::OK);
    assert_eq!(
        send(&app, list()).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn the_health_check_is_never_limited() {
    let one_a_minute = Some(Rate::new(1, Duration::from_secs(60)));
    let app = app(limited(one_a_minute, one_a_minute)).await;

    for _ in 0..5 {
        let reply = send(
            &app,
            request(Method::GET, "/health", VISITOR)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);
    }
}

#[tokio::test]
async fn a_forged_forwarding_header_buys_no_extra_budget() {
    let app = app(limited(Some(Rate::new(1, Duration::from_secs(60))), None)).await;

    for forged in ["192.0.2.1", "192.0.2.2"] {
        let mut request = new_entry(VISITOR);
        request
            .headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static(forged));
        let status = send(&app, request).await.status;
        if forged == "192.0.2.1" {
            assert_eq!(status, StatusCode::CREATED);
        } else {
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        }
    }
}

#[tokio::test]
async fn behind_a_trusted_proxy_each_visitor_has_a_budget_of_their_own() {
    let app = app(limited(Some(Rate::new(1, Duration::from_secs(60))), None)).await;

    for visitor in ["192.0.2.1", "192.0.2.2"] {
        let mut request = new_entry(PROXY);
        request
            .headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static(visitor));
        assert_eq!(send(&app, request).await.status, StatusCode::CREATED);
    }

    let mut again = new_entry(PROXY);
    again
        .headers_mut()
        .insert("x-forwarded-for", HeaderValue::from_static("192.0.2.1"));
    assert_eq!(
        send(&app, again).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

// ---------- limits on the request itself ----------

#[tokio::test]
async fn a_body_over_the_size_limit_is_refused() {
    let app = app(HttpConfig {
        max_body_bytes: 64,
        ..unlimited()
    })
    .await;

    let request = request(Method::POST, "/api/entries", VISITOR)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"body": "x".repeat(100)}).to_string()))
        .unwrap();
    let reply = send(&app, request).await;

    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(reply.body["error"], "payload_too_large");
    assert_eq!(reply.body["reason"], "body_too_large");
}

#[tokio::test]
async fn entry_limits_come_from_the_configuration() {
    let limits = EntryLimits {
        author_chars: 5,
        max_per_page: 3,
        default_per_page: 2,
        ..EntryLimits::default()
    };
    let app = app_with(unlimited(), limits).await;

    let named = request(Method::POST, "/api/entries", VISITOR)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({"author": "marina", "body": "hi"}).to_string(),
        ))
        .unwrap();
    let reply = send(&app, named).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        reply.body["message"],
        "The name is longer than 5 characters."
    );

    let page = |uri: &'static str| {
        request(Method::GET, uri, VISITOR)
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(send(&app, page("/api/entries")).await.body["per_page"], 2);
    assert_eq!(
        send(&app, page("/api/entries?per_page=50")).await.body["per_page"],
        3
    );
}

// ---------- every error is JSON ----------

#[tokio::test]
async fn an_unknown_address_is_a_json_404() {
    let app = app(unlimited()).await;

    let reply = send(
        &app,
        request(Method::GET, "/wp-login.php", VISITOR)
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body["error"], "not_found");
    assert_eq!(reply.body["reason"], "not_found");
}

#[tokio::test]
async fn a_wrong_method_is_a_json_405_that_still_says_what_is_allowed() {
    let app = app(unlimited()).await;

    let reply = send(
        &app,
        request(Method::DELETE, "/api/entries", VISITOR)
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(reply.body["error"], "method_not_allowed");
    assert_eq!(reply.body["reason"], "method_not_allowed");
    let allow = reply.headers[header::ALLOW].to_str().unwrap();
    assert!(allow.contains("POST") && allow.contains("GET"), "{allow}");
}

#[tokio::test]
async fn a_malformed_query_is_a_json_400() {
    let app = app(unlimited()).await;

    let reply = send(
        &app,
        request(Method::GET, "/api/entries?page=abc", VISITOR)
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(reply.body["error"], "bad_request");
    assert_eq!(reply.body["reason"], "malformed_query");
}

#[tokio::test]
async fn a_body_not_declared_as_json_is_a_json_415() {
    let app = app(unlimited()).await;

    let reply = send(
        &app,
        request(Method::POST, "/api/entries", VISITOR)
            .body(Body::from(json!({"body": "hi"}).to_string()))
            .unwrap(),
    )
    .await;

    assert_eq!(reply.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(reply.body["error"], "unsupported_media_type");
    assert_eq!(reply.body["reason"], "unsupported_content_type");
}

#[tokio::test]
async fn a_slow_handler_is_cut_off_with_a_json_503() {
    let app = Router::new()
        .route(
            "/slow",
            get(|| async { tokio::time::sleep(Duration::from_secs(5)).await }),
        )
        .layer(axum::middleware::from_fn_with_state(
            Duration::from_millis(50),
            middleware::timeout,
        ));

    let reply = send(&app, Request::get("/slow").body(Body::empty()).unwrap()).await;

    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.body["error"], "timeout");
    assert_eq!(reply.body["reason"], "timeout");
}

#[tokio::test]
async fn a_panicking_handler_is_a_json_500() {
    let app = Router::new()
        .route("/boom", get(on_fire))
        .layer(CatchPanicLayer::custom(middleware::panic_response));

    let reply = send(&app, Request::get("/boom").body(Body::empty()).unwrap()).await;

    assert_eq!(reply.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(reply.body["error"], "internal_error");
    assert_eq!(reply.body["reason"], "internal");
}

async fn on_fire() -> &'static str {
    panic!("the ashtray is on fire")
}

// ---------- headers on every response ----------

#[tokio::test]
async fn every_response_gets_a_request_id() {
    let app = app(unlimited()).await;

    let reply = send(
        &app,
        request(Method::GET, "/health", VISITOR)
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    let id = reply.headers["x-request-id"].to_str().unwrap();
    assert_eq!(id.len(), 36, "a UUID: {id}");
}

#[tokio::test]
async fn an_incoming_request_id_is_kept() {
    let app = app(unlimited()).await;

    let reply = send(
        &app,
        request(Method::GET, "/health", VISITOR)
            .header("x-request-id", "from-nginx-42")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(reply.headers["x-request-id"], "from-nginx-42");
}

#[tokio::test]
async fn responses_cannot_be_sniffed_or_framed() {
    let app = app(unlimited()).await;

    let reply = send(
        &app,
        request(Method::GET, "/api/entries", VISITOR)
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(reply.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(reply.headers[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(reply.headers[header::REFERRER_POLICY], "no-referrer");
    assert!(reply.headers.contains_key(header::CONTENT_SECURITY_POLICY));
}
