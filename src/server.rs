//! The accept loop.
//!
//! `axum::serve` would do, except for two things a public endpoint needs: a
//! deadline on reading request headers, so a client trickling one byte a
//! minute cannot hold a connection forever, and a deadline on the graceful
//! shutdown, so one stuck request cannot hold up a restart until the
//! orchestrator loses patience and kills the process mid-write.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::pin;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::config::ServerConfig;

/// How long to back off when `accept` fails for a reason that is not about a
/// single connection — typically running out of file descriptors.
const ACCEPT_BACKOFF: Duration = Duration::from_secs(1);

/// Serves `app` on `listener` until `shutdown` resolves, then waits up to
/// `config.shutdown_timeout` for in-flight requests before returning.
///
/// Every request carries the peer address as `ConnectInfo<SocketAddr>`.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    config: &ServerConfig,
    shutdown: impl Future<Output = ()>,
) {
    let mut builder = auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(config.header_read_timeout);

    let graceful = GracefulShutdown::new();
    let mut shutdown = pin!(shutdown);

    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(cause) => {
                    if !is_connection_error(&cause) {
                        tracing::error!(%cause, "cannot accept connections, backing off");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                    }
                    continue;
                }
            },
            () = &mut shutdown => break,
        };

        if let Err(cause) = stream.set_nodelay(true) {
            tracing::debug!(%peer, %cause, "cannot set TCP_NODELAY");
        }

        let service = app
            .clone()
            .map_request(move |mut request: Request<Incoming>| {
                request.extensions_mut().insert(ConnectInfo(peer));
                request.map(Body::new)
            });

        let connection = builder
            .serve_connection_with_upgrades(TokioIo::new(stream), TowerToHyperService::new(service))
            .into_owned();
        let connection = graceful.watch(connection);

        tokio::spawn(async move {
            if let Err(cause) = connection.await {
                tracing::debug!(%peer, %cause, "connection ended with an error");
            }
        });
    }

    // Stop taking connections before waiting on the ones already open, so a
    // load balancer sees the port close and moves on.
    drop(listener);
    tracing::info!(
        timeout = ?config.shutdown_timeout,
        "no longer accepting connections, waiting for open requests"
    );

    tokio::select! {
        () = graceful.shutdown() => tracing::info!("every request finished"),
        () = tokio::time::sleep(config.shutdown_timeout) => {
            tracing::warn!("requests still open after the shutdown timeout, dropping them");
        }
    }
}

/// Errors that concern one connection, which the next `accept` is not
/// affected by.
fn is_connection_error(cause: &io::Error) -> bool {
    matches!(
        cause.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    )
}

/// The address to reach a server bound to `bind_addr` from the same host: a
/// wildcard bind is reachable on loopback.
pub fn local_address(bind_addr: SocketAddr) -> SocketAddr {
    let mut addr = bind_addr;
    if addr.ip().is_unspecified() {
        addr.set_ip(match addr {
            SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
            SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
        });
    }
    addr
}
