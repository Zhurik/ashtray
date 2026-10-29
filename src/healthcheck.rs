//! `ashtray healthcheck`: asks a running instance whether it is healthy.
//!
//! The image's `HEALTHCHECK` runs this instead of `curl`, so the runtime
//! image needs nothing but the binary. It speaks just enough HTTP/1.1 to send
//! one request and read one status line.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The longest status line worth reading: `HTTP/1.1 200 OK\r\n` and then some.
const STATUS_LINE_LIMIT: u64 = 256;

#[derive(Debug, thiserror::Error)]
pub enum HealthcheckError {
    #[error("no answer from {0} within {1:?}")]
    Timeout(SocketAddr, Duration),
    #[error("cannot reach {0}: {1}")]
    Io(SocketAddr, std::io::Error),
    #[error("{0} is unhealthy: {1}")]
    Unhealthy(SocketAddr, String),
}

/// Succeeds when `GET /health` on `addr` answers `200`.
pub async fn probe(addr: SocketAddr, timeout: Duration) -> Result<(), HealthcheckError> {
    tokio::time::timeout(timeout, request(addr))
        .await
        .map_err(|_| HealthcheckError::Timeout(addr, timeout))?
}

async fn request(addr: SocketAddr) -> Result<(), HealthcheckError> {
    let io = |cause| HealthcheckError::Io(addr, cause);

    let mut stream = TcpStream::connect(addr).await.map_err(io)?;
    stream
        .write_all(
            format!("GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .map_err(io)?;

    let mut head = Vec::new();
    stream
        .take(STATUS_LINE_LIMIT)
        .read_to_end(&mut head)
        .await
        .map_err(io)?;

    let head = String::from_utf8_lossy(&head);
    let status_line = head.lines().next().unwrap_or_default();
    match status_line.split_whitespace().nth(1) {
        Some("200") => Ok(()),
        _ => Err(HealthcheckError::Unhealthy(addr, status_line.to_owned())),
    }
}
