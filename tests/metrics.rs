//! `/metrics`: what it counts, how it labels, and who may read it.

use std::sync::Arc;
use std::time::Duration;

use ashtray::config::{HttpConfig, Rate, RateLimitConfig};
use ashtray::models::EntryLimits;
use ashtray::{AppState, db, metrics, router};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;

const TOKEN: &str = "scrape-me-gently";

struct Service {
    api: Router,
    metrics: Router,
}

async fn service(rate_limit: RateLimitConfig, token: Option<&str>) -> Service {
    let pool = db::connect("sqlite::memory:", 1)
        .await
        .expect("in-memory database");
    let state = AppState::new(pool, EntryLimits::default());
    let http = HttpConfig {
        rate_limit,
        ..HttpConfig::default()
    };
    Service {
        api: router(state.clone(), &http),
        metrics: metrics::router(state, token.map(Arc::from)),
    }
}

async fn call(app: &Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn post_entry(app: &Router) -> StatusCode {
    let request = Request::post("/api/entries")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"body": "hi"}).to_string()))
        .unwrap();
    call(app, request).await.0
}

async fn scrape(service: &Service) -> String {
    let (status, body) = call(
        &service.metrics,
        Request::get("/metrics").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// The value of the sample line that starts with `series`.
fn sample(body: &str, series: &str) -> Option<f64> {
    body.lines()
        .find_map(|line| line.strip_prefix(series)?.trim().parse().ok())
}

#[tokio::test]
async fn requests_are_counted_by_route_and_status() {
    let service = service(RateLimitConfig::disabled(), None).await;

    post_entry(&service.api).await;
    post_entry(&service.api).await;
    call(
        &service.api,
        Request::get("/api/entries").body(Body::empty()).unwrap(),
    )
    .await;

    let body = scrape(&service).await;

    assert_eq!(
        sample(
            &body,
            r#"http_requests_total{method="POST",route="/api/entries",status="201"}"#
        ),
        Some(2.0),
        "{body}"
    );
    assert_eq!(
        sample(
            &body,
            r#"http_requests_total{method="GET",route="/api/entries",status="200"}"#
        ),
        Some(1.0)
    );
    assert_eq!(
        sample(
            &body,
            r#"http_request_duration_seconds_count{method="POST",route="/api/entries"}"#
        ),
        Some(2.0)
    );
    assert_eq!(sample(&body, "http_requests_in_flight"), Some(0.0));
}

#[tokio::test]
async fn unrouted_paths_share_one_label() {
    let service = service(RateLimitConfig::disabled(), None).await;

    for path in ["/wp-login.php", "/.env", "/admin"] {
        call(
            &service.api,
            Request::get(path).body(Body::empty()).unwrap(),
        )
        .await;
    }

    let body = scrape(&service).await;

    assert_eq!(
        sample(
            &body,
            r#"http_requests_total{method="GET",route="unmatched",status="404"}"#
        ),
        Some(3.0),
        "{body}"
    );
    assert!(!body.contains("wp-login"));
}

#[tokio::test]
async fn entries_are_counted_written_and_stored() {
    let service = service(RateLimitConfig::disabled(), None).await;

    post_entry(&service.api).await;
    post_entry(&service.api).await;
    post_entry(&service.api).await;

    let body = scrape(&service).await;

    assert_eq!(sample(&body, "ashtray_entries_created_total"), Some(3.0));
    assert_eq!(sample(&body, "ashtray_entries "), Some(3.0), "{body}");
    assert_eq!(sample(&body, "ashtray_database_up"), Some(1.0));
    assert!(sample(&body, "ashtray_db_connections ").is_some(), "{body}");
}

#[tokio::test]
async fn refusals_by_the_rate_limit_are_counted() {
    let rate_limit = RateLimitConfig {
        post: Some(Rate::new(1, Duration::from_secs(60))),
        get: None,
    };
    let service = service(rate_limit, None).await;

    assert_eq!(post_entry(&service.api).await, StatusCode::CREATED);
    assert_eq!(
        post_entry(&service.api).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        post_entry(&service.api).await,
        StatusCode::TOO_MANY_REQUESTS
    );

    let body = scrape(&service).await;

    assert_eq!(
        sample(
            &body,
            r#"ashtray_rate_limited_requests_total{method="POST"}"#
        ),
        Some(2.0),
        "{body}"
    );
}

#[tokio::test]
async fn the_standard_series_are_there() {
    let service = service(RateLimitConfig::disabled(), None).await;

    let body = scrape(&service).await;

    assert!(body.contains(r#"ashtray_build_info{version=""#), "{body}");
    assert!(sample(&body, "process_start_time_seconds").is_some());
    if cfg!(target_os = "linux") {
        assert!(
            sample(&body, "process_resident_memory_bytes").is_some(),
            "{body}"
        );
        assert!(sample(&body, "process_cpu_seconds_total").is_some());
        assert!(sample(&body, "process_open_fds").is_some());
    }
    assert!(body.trim_end().ends_with("# EOF"));
}

#[tokio::test]
async fn the_answer_is_openmetrics() {
    let service = service(RateLimitConfig::disabled(), None).await;

    let response = service
        .metrics
        .clone()
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let content_type = response.headers()[header::CONTENT_TYPE].to_str().unwrap();
    assert!(content_type.starts_with("application/openmetrics-text"));
}

#[tokio::test]
async fn a_token_keeps_strangers_out() {
    let service = service(RateLimitConfig::disabled(), Some(TOKEN)).await;
    let with = |authorization: Option<&str>| {
        let mut request = Request::get("/metrics");
        if let Some(authorization) = authorization {
            request = request.header(header::AUTHORIZATION, authorization);
        }
        request.body(Body::empty()).unwrap()
    };

    let (status, body) = call(&service.metrics, with(None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("unauthorized"), "{body}");

    let (status, _) = call(&service.metrics, with(Some("Bearer wrong"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = call(&service.metrics, with(Some(&format!("Bearer {TOKEN}")))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("http_requests_in_flight"));
}

#[tokio::test]
async fn the_api_does_not_serve_metrics() {
    let service = service(RateLimitConfig::disabled(), None).await;

    let (status, _) = call(
        &service.api,
        Request::get("/metrics").body(Body::empty()).unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
}
