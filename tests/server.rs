//! The real listener, over real TCP: deadlines, the peer address, shutdown
//! and the `healthcheck` probe.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use ashtray::config::{HttpConfig, RateLimitConfig, ServerConfig};
use ashtray::models::EntryLimits;
use ashtray::{AppState, db, healthcheck, router, server};
use axum::Router;
use axum::extract::ConnectInfo;
use axum::routing::get;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

struct Running {
    addr: SocketAddr,
    stop: oneshot::Sender<()>,
    done: JoinHandle<()>,
}

async fn start(app: Router, config: ServerConfig) -> Running {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("address");
    let (stop, stopped) = oneshot::channel::<()>();

    let done = tokio::spawn(async move {
        server::serve(listener, app, &config, async {
            let _ = stopped.await;
        })
        .await;
    });

    Running { addr, stop, done }
}

async fn ashtray() -> Router {
    let pool = db::connect("sqlite::memory:", 1)
        .await
        .expect("in-memory database");
    let http = HttpConfig {
        rate_limit: RateLimitConfig::disabled(),
        ..HttpConfig::default()
    };
    router(AppState::new(pool, EntryLimits::default()), &http)
}

fn quick() -> ServerConfig {
    ServerConfig {
        header_read_timeout: Duration::from_millis(200),
        shutdown_timeout: Duration::from_millis(300),
    }
}

/// Sends `raw` and reads until the server closes the connection.
async fn exchange(addr: SocketAddr, raw: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream.write_all(raw.as_bytes()).await.expect("write");
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).await.expect("read");
    String::from_utf8_lossy(&answer).into_owned()
}

#[tokio::test]
async fn the_healthcheck_probe_passes_against_a_healthy_server() {
    let running = start(ashtray().await, quick()).await;

    healthcheck::probe(running.addr, Duration::from_secs(2))
        .await
        .expect("healthy");
}

#[tokio::test]
async fn the_healthcheck_probe_fails_when_nobody_listens() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    assert!(
        healthcheck::probe(addr, Duration::from_secs(2))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn handlers_see_the_tcp_peer() {
    let app = Router::new().route(
        "/whoami",
        get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
    );
    let running = start(app, quick()).await;

    let answer = exchange(
        running.addr,
        "GET /whoami HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n",
    )
    .await;

    assert!(answer.ends_with("127.0.0.1"), "{answer}");
}

#[tokio::test]
async fn a_client_that_never_finishes_its_headers_is_dropped() {
    let running = start(ashtray().await, quick()).await;
    let started = Instant::now();

    let answer = exchange(running.addr, "GET /health HTTP/1.1\r\nHost: te").await;

    let waited = started.elapsed();
    assert!(!answer.contains("200"), "{answer}");
    assert!(
        waited >= Duration::from_millis(200),
        "dropped too early: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(2),
        "dropped too late: {waited:?}"
    );
}

#[tokio::test]
async fn an_idle_keep_alive_connection_is_closed() {
    let running = start(ashtray().await, quick()).await;
    let mut stream = TcpStream::connect(running.addr).await.unwrap();

    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .unwrap();
    let mut buffer = [0; 1024];
    let read = stream.read(&mut buffer).await.unwrap();
    assert!(String::from_utf8_lossy(&buffer[..read]).starts_with("HTTP/1.1 200"));

    // The connection stays open for another request, but not forever.
    let started = Instant::now();
    let closed = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buffer))
        .await
        .expect("closed within the timeout")
        .unwrap();

    assert_eq!(closed, 0, "the server should have hung up");
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn shutdown_gives_up_on_a_stuck_request() {
    let app = Router::new().route(
        "/stuck",
        get(|| async { tokio::time::sleep(Duration::from_secs(30)).await }),
    );
    let running = start(app, quick()).await;

    let mut stream = TcpStream::connect(running.addr).await.unwrap();
    stream
        .write_all(b"GET /stuck HTTP/1.1\r\nHost: test\r\n\r\n")
        .await
        .unwrap();
    // Let the request reach the handler before pulling the plug.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = Instant::now();
    running.stop.send(()).unwrap();
    running.done.await.unwrap();

    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(300), "{waited:?}");
    assert!(waited < Duration::from_secs(2), "{waited:?}");
}

#[tokio::test]
async fn shutdown_lets_a_finished_server_go_at_once() {
    let running = start(ashtray().await, quick()).await;
    healthcheck::probe(running.addr, Duration::from_secs(2))
        .await
        .unwrap();

    let started = Instant::now();
    running.stop.send(()).unwrap();
    running.done.await.unwrap();

    assert!(started.elapsed() < Duration::from_millis(300));
    assert!(TcpStream::connect(running.addr).await.is_err());
}
