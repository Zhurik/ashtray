use std::io::IsTerminal;
use std::process::ExitCode;
use std::time::Duration;

use ashtray::config::{Config, LogConfig, LogFormat};
use ashtray::{AppState, db, healthcheck, router, server};
use tokio::net::TcpListener;
use tokio::signal;
use tracing_subscriber::EnvFilter;

/// Exit code for a configuration that cannot be used, as opposed to a failure
/// while running. Matches `EX_CONFIG` from `sysexits.h`.
const EXIT_CONFIG: u8 = 78;

const HEALTHCHECK_TIMEOUT: Duration = Duration::from_secs(3);

const USAGE: &str = "\
usage: ashtray [command]

commands:
  (none)        run the server
  healthcheck   exit 0 if the server on ASHTRAY_BIND_ADDR answers /health
  version       print the version

Every setting is an ASHTRAY_* environment variable; see the README.";

#[tokio::main]
async fn main() -> ExitCode {
    let command = std::env::args().nth(1);
    match command.as_deref() {
        None => run().await,
        Some("healthcheck") => check_health().await,
        Some("version" | "--version" | "-V") => {
            println!("ashtray {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("ashtray: unknown command `{other}`\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

async fn run() -> ExitCode {
    let log = match LogConfig::from_env() {
        Ok(log) => log,
        Err(error) => {
            // No log yet to report through.
            eprintln!("ashtray: {error}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    init_tracing(&log);

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    for warning in config.warnings() {
        tracing::warn!("{warning}");
    }

    match serve(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let pool = db::connect(&config.database_url, config.max_connections).await?;
    let listener = TcpListener::bind(config.bind_addr).await?;

    let http = &config.http;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        address = %listener.local_addr()?,
        database = %config.database_url,
        cors = ?http.cors.as_ref().map(|cors| &cors.origins),
        trusted_proxies = ?http.client_ip.trusted_proxies,
        client_ip_header = %http.client_ip.header,
        rate_limit_post = %display_rate(http.rate_limit.post),
        rate_limit_get = %display_rate(http.rate_limit.get),
        max_body_bytes = http.max_body_bytes,
        request_timeout = ?http.request_timeout,
        "ashtray is open"
    );

    let app = router(AppState::new(pool.clone(), config.limits), http);
    server::serve(listener, app, &config.server, shutdown_signal()).await;

    // Checkpoints the WAL into the main file, so a backup of the volume taken
    // after a clean stop is a single self-contained file.
    pool.close().await;
    tracing::info!("closed");

    Ok(())
}

async fn check_health() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("ashtray: {error}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    match healthcheck::probe(server::local_address(config.bind_addr), HEALTHCHECK_TIMEOUT).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ashtray: {error}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing(log: &LogConfig) {
    let filter = EnvFilter::new(&log.filter);
    // Colors only for a person at a terminal; `docker logs` and journald
    // would otherwise keep the escape codes.
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stdout().is_terminal());
    match log.format {
        LogFormat::Text => builder.init(),
        LogFormat::Json => builder
            .json()
            .flatten_event(true)
            .with_current_span(true)
            .with_span_list(false)
            .init(),
    }
}

fn display_rate(rate: Option<ashtray::config::Rate>) -> String {
    rate.map_or_else(|| "off".to_owned(), |rate| rate.to_string())
}

/// Stops accepting new requests on Ctrl-C or SIGTERM, so a container restart
/// does not cut an in-flight write in half.
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("cannot listen for Ctrl-C");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("cannot listen for SIGTERM")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    tracing::info!("shutting down");
}
