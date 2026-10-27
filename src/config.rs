use std::env;
use std::net::SocketAddr;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";
const DEFAULT_DATABASE_URL: &str = "sqlite://ashtray.db";
const DEFAULT_MAX_CONNECTIONS: u32 = 5;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub database_url: String,
    pub max_connections: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("ASHTRAY_BIND_ADDR is not a socket address: {0}")]
    BindAddr(String),
    #[error("ASHTRAY_MAX_CONNECTIONS is not a positive number: {0}")]
    MaxConnections(String),
}

impl Config {
    /// Reads the configuration from the environment, falling back to defaults
    /// that are safe to run locally without any setup.
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind_addr =
            env::var("ASHTRAY_BIND_ADDR").unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_owned());
        let bind_addr = bind_addr
            .parse::<SocketAddr>()
            .map_err(|_| ConfigError::BindAddr(bind_addr))?;

        let database_url =
            env::var("ASHTRAY_DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());

        let max_connections = match env::var("ASHTRAY_MAX_CONNECTIONS") {
            Ok(raw) => raw
                .parse::<u32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or(ConfigError::MaxConnections(raw))?,
            Err(_) => DEFAULT_MAX_CONNECTIONS,
        };

        Ok(Self {
            bind_addr,
            database_url,
            max_connections,
        })
    }
}
