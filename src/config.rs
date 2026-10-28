//! Everything the service reads from its environment.
//!
//! Every knob is an `ASHTRAY_*` variable with a default that is safe to run
//! locally. A variable that is unset or set to an empty string takes its
//! default, so Compose files can pass `${VAR:-}` through without having to
//! repeat the defaults. A variable that is set but cannot be parsed stops the
//! process at startup — every such variable is reported at once, not just the
//! first one.

mod parse;

use std::env;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::time::Duration;

use axum::http::{HeaderName, HeaderValue};
use ipnet::IpNet;

use crate::models::EntryLimits;

const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";
const DEFAULT_DATABASE_URL: &str = "sqlite://ashtray.db";
const DEFAULT_MAX_CONNECTIONS: u32 = 5;
const DEFAULT_LOG_FILTER: &str = "info";

/// Worst case for one character of an entry once it is JSON-encoded: four
/// bytes of UTF-8. Used only to warn about a body limit that is too tight.
const MAX_BYTES_PER_CHAR: usize = 4;
/// Room for the keys, quotes and braces around the three fields.
const JSON_OVERHEAD_BYTES: usize = 128;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub database_url: String,
    pub max_connections: u32,
    pub server: ServerConfig,
    pub http: HttpConfig,
    pub limits: EntryLimits,
}

/// How the listener treats connections, as opposed to the requests on them.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// How long a client gets to send the request headers. Bounds slow-loris
    /// clients and idle keep-alive connections alike.
    pub header_read_timeout: Duration,
    /// How long in-flight requests get to finish after SIGTERM before the
    /// remaining connections are dropped.
    pub shutdown_timeout: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            header_read_timeout: Duration::from_secs(10),
            // Docker sends SIGKILL ten seconds after SIGTERM; finishing first
            // leaves time to log the shutdown.
            shutdown_timeout: Duration::from_secs(8),
        }
    }
}

/// Everything the HTTP layers in front of the handlers need.
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// `None` leaves CORS off: no cross-origin script can read a response.
    pub cors: Option<CorsConfig>,
    pub client_ip: ClientIpConfig,
    pub rate_limit: RateLimitConfig,
    pub max_body_bytes: usize,
    pub request_timeout: Duration,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            cors: None,
            client_ip: ClientIpConfig::default(),
            rate_limit: RateLimitConfig::default(),
            max_body_bytes: 16 * 1024,
            request_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CorsConfig {
    pub origins: AllowedOrigins,
    /// How long a browser may cache the answer to a preflight request.
    pub max_age: Duration,
}

const DEFAULT_CORS_MAX_AGE: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowedOrigins {
    /// `*`: any page on any site may call the API.
    Any,
    /// Exact origins, already normalized to what a browser sends in `Origin`.
    List(Vec<HeaderValue>),
}

/// Who is allowed to tell the service what the client's address is.
#[derive(Debug, Clone)]
pub struct ClientIpConfig {
    /// Peers whose forwarding header is believed. Empty means nobody's is,
    /// and the client is always the TCP peer.
    pub trusted_proxies: Vec<IpNet>,
    /// The header those peers put the client address in.
    pub header: HeaderName,
}

impl Default for ClientIpConfig {
    fn default() -> Self {
        Self {
            trusted_proxies: Vec::new(),
            header: HeaderName::from_static("x-forwarded-for"),
        }
    }
}

impl ClientIpConfig {
    pub fn trusts(&self, peer: IpAddr) -> bool {
        self.trusted_proxies.iter().any(|net| net.contains(&peer))
    }
}

/// Per-client request budgets. `None` turns the limit for that method off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitConfig {
    pub post: Option<Rate>,
    pub get: Option<Rate>,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            post: Some(Rate::new(10, Duration::from_secs(60 * 60))),
            get: Some(Rate::new(120, Duration::from_secs(60))),
        }
    }
}

impl RateLimitConfig {
    /// No limits at all — for tests and for running behind something that
    /// already limits.
    pub fn disabled() -> Self {
        Self {
            post: None,
            get: None,
        }
    }
}

/// `count` requests per `per`, refilled evenly: `10/1h` lets a client spend
/// ten at once and then earns one back every six minutes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub count: NonZeroU32,
    pub per: Duration,
}

impl Rate {
    /// # Panics
    ///
    /// On a zero count or a zero period, which only a typo in the code can
    /// produce: rates from the environment go through the parser instead.
    pub fn new(count: u32, per: Duration) -> Self {
        assert!(!per.is_zero(), "a rate needs a period");
        Self {
            count: NonZeroU32::new(count).expect("a rate needs a count"),
            per,
        }
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.count, humantime::format_duration(self.per))
    }
}

/// How the log is written. Read before everything else, so that the rest of
/// the configuration can already be reported through it.
#[derive(Debug, Clone)]
pub struct LogConfig {
    pub filter: String,
    pub format: LogFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable lines, for a terminal.
    Text,
    /// One JSON object per line, for a log collector.
    Json,
}

/// One variable that is set to something unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidVar {
    pub var: &'static str,
    pub value: String,
    pub reason: String,
}

impl fmt::Display for InvalidVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={:?}: {}", self.var, self.value, self.reason)
    }
}

/// Every unusable variable found in one pass over the environment.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ConfigError(pub Vec<InvalidVar>);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid configuration")?;
        for invalid in &self.0 {
            write!(f, "\n  {invalid}")?;
        }
        Ok(())
    }
}

/// Reads variables through `lookup` and collects every failure instead of
/// stopping at the first one.
struct Reader<'a> {
    lookup: &'a dyn Fn(&str) -> Option<String>,
    invalid: Vec<InvalidVar>,
}

impl<'a> Reader<'a> {
    fn new(lookup: &'a dyn Fn(&str) -> Option<String>) -> Self {
        Self {
            lookup,
            invalid: Vec::new(),
        }
    }

    fn read<T>(
        &mut self,
        var: &'static str,
        default: T,
        parse: impl FnOnce(&str) -> Result<T, String>,
    ) -> T {
        let Some(value) = (self.lookup)(var) else {
            return default;
        };
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return default;
        }
        match parse(trimmed) {
            Ok(parsed) => parsed,
            Err(reason) => {
                self.reject(var, value, reason);
                default
            }
        }
    }

    fn reject(&mut self, var: &'static str, value: String, reason: String) {
        self.invalid.push(InvalidVar { var, value, reason });
    }

    fn raw(&self, var: &str) -> String {
        (self.lookup)(var).unwrap_or_default()
    }

    fn finish<T>(self, value: T) -> Result<T, ConfigError> {
        if self.invalid.is_empty() {
            Ok(value)
        } else {
            Err(ConfigError(self.invalid))
        }
    }
}

fn from_process_env(var: &str) -> Option<String> {
    env::var(var).ok()
}

impl LogConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&from_process_env)
    }

    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut reader = Reader::new(lookup);
        let config = Self {
            filter: reader.read(
                "ASHTRAY_LOG",
                DEFAULT_LOG_FILTER.to_owned(),
                parse::log_filter,
            ),
            format: reader.read("ASHTRAY_LOG_FORMAT", LogFormat::Text, parse::log_format),
        };
        reader.finish(config)
    }
}

impl Config {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&from_process_env)
    }

    /// Reads the configuration through `lookup`, which answers the value of a
    /// variable or `None` when it is unset.
    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut reader = Reader::new(lookup);

        let bind_addr = reader.read(
            "ASHTRAY_BIND_ADDR",
            DEFAULT_BIND_ADDR.parse().expect("default bind address"),
            parse::socket_addr,
        );
        let database_url = reader.read(
            "ASHTRAY_DATABASE_URL",
            DEFAULT_DATABASE_URL.to_owned(),
            |raw| Ok(raw.to_owned()),
        );
        let max_connections = reader.read(
            "ASHTRAY_MAX_CONNECTIONS",
            DEFAULT_MAX_CONNECTIONS,
            parse::positive,
        );

        let server = Self::read_server(&mut reader);
        let http = Self::read_http(&mut reader);
        let limits = Self::read_limits(&mut reader);

        reader.finish(Self {
            bind_addr,
            database_url,
            max_connections,
            server,
            http,
            limits,
        })
    }

    fn read_server(reader: &mut Reader<'_>) -> ServerConfig {
        let defaults = ServerConfig::default();
        ServerConfig {
            header_read_timeout: reader.read(
                "ASHTRAY_HEADER_READ_TIMEOUT",
                defaults.header_read_timeout,
                parse::duration,
            ),
            shutdown_timeout: reader.read(
                "ASHTRAY_SHUTDOWN_TIMEOUT",
                defaults.shutdown_timeout,
                parse::duration,
            ),
        }
    }

    fn read_http(reader: &mut Reader<'_>) -> HttpConfig {
        let defaults = HttpConfig::default();

        let origins = reader.read("ASHTRAY_CORS_ALLOWED_ORIGINS", None, |raw| {
            parse::origins(raw).map(Some)
        });
        let max_age = reader.read(
            "ASHTRAY_CORS_MAX_AGE",
            DEFAULT_CORS_MAX_AGE,
            parse::duration,
        );
        let cors = origins.map(|origins| CorsConfig { origins, max_age });

        let client_ip = ClientIpConfig {
            trusted_proxies: reader.read(
                "ASHTRAY_TRUSTED_PROXIES",
                defaults.client_ip.trusted_proxies,
                parse::networks,
            ),
            header: reader.read(
                "ASHTRAY_CLIENT_IP_HEADER",
                defaults.client_ip.header,
                parse::header_name,
            ),
        };

        let rate_limit = RateLimitConfig {
            post: reader.read(
                "ASHTRAY_RATE_LIMIT_POST",
                defaults.rate_limit.post,
                parse::rate,
            ),
            get: reader.read(
                "ASHTRAY_RATE_LIMIT_GET",
                defaults.rate_limit.get,
                parse::rate,
            ),
        };

        HttpConfig {
            cors,
            client_ip,
            rate_limit,
            max_body_bytes: reader.read(
                "ASHTRAY_MAX_BODY_SIZE",
                defaults.max_body_bytes,
                parse::byte_size,
            ),
            request_timeout: reader.read(
                "ASHTRAY_REQUEST_TIMEOUT",
                defaults.request_timeout,
                parse::duration,
            ),
        }
    }

    fn read_limits(reader: &mut Reader<'_>) -> EntryLimits {
        let defaults = EntryLimits::default();
        let limits = EntryLimits {
            author_chars: reader.read(
                "ASHTRAY_MAX_AUTHOR_CHARS",
                defaults.author_chars,
                parse::positive,
            ),
            url_chars: reader.read("ASHTRAY_MAX_URL_CHARS", defaults.url_chars, parse::positive),
            body_chars: reader.read(
                "ASHTRAY_MAX_BODY_CHARS",
                defaults.body_chars,
                parse::positive,
            ),
            default_per_page: reader.read(
                "ASHTRAY_DEFAULT_PER_PAGE",
                defaults.default_per_page,
                parse::positive,
            ),
            max_per_page: reader.read(
                "ASHTRAY_MAX_PER_PAGE",
                defaults.max_per_page,
                parse::positive,
            ),
        };

        if limits.default_per_page > limits.max_per_page {
            let value = reader.raw("ASHTRAY_DEFAULT_PER_PAGE");
            reader.reject(
                "ASHTRAY_DEFAULT_PER_PAGE",
                value,
                format!(
                    "is larger than ASHTRAY_MAX_PER_PAGE ({})",
                    limits.max_per_page
                ),
            );
        }

        limits
    }

    /// Settings that are valid on their own but probably not what was meant.
    /// They are logged at startup and do not stop the process.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        let largest_entry = MAX_BYTES_PER_CHAR
            * (self.limits.author_chars + self.limits.url_chars + self.limits.body_chars)
            + JSON_OVERHEAD_BYTES;
        if self.http.max_body_bytes < largest_entry {
            warnings.push(format!(
                "ASHTRAY_MAX_BODY_SIZE is {} bytes, but an entry at the character limits can take \
                 up to {largest_entry}; long non-ASCII entries will be refused as too large",
                self.http.max_body_bytes
            ));
        }

        warnings
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        Config::from_lookup(&|var| vars.get(var).cloned())
    }

    fn rejected(vars: &[(&str, &str)]) -> Vec<&'static str> {
        config(vars)
            .expect_err("configuration should be refused")
            .0
            .into_iter()
            .map(|invalid| invalid.var)
            .collect()
    }

    #[test]
    fn an_empty_environment_gives_the_defaults() {
        let config = config(&[]).expect("defaults");

        assert_eq!(config.bind_addr, DEFAULT_BIND_ADDR.parse().unwrap());
        assert!(config.http.cors.is_none());
        assert!(config.http.client_ip.trusted_proxies.is_empty());
        assert_eq!(config.http.rate_limit, RateLimitConfig::default());
        assert_eq!(config.http.max_body_bytes, 16 * 1024);
        assert_eq!(config.limits, EntryLimits::default());
        assert!(config.warnings().is_empty());
    }

    #[test]
    fn an_empty_variable_counts_as_unset() {
        let config = config(&[
            ("ASHTRAY_RATE_LIMIT_POST", ""),
            ("ASHTRAY_CORS_ALLOWED_ORIGINS", "  "),
        ])
        .expect("defaults");

        assert_eq!(config.http.rate_limit.post, RateLimitConfig::default().post);
        assert!(config.http.cors.is_none());
    }

    #[test]
    fn every_invalid_variable_is_reported_at_once() {
        let vars = rejected(&[
            ("ASHTRAY_BIND_ADDR", "localhost"),
            ("ASHTRAY_MAX_CONNECTIONS", "0"),
            ("ASHTRAY_REQUEST_TIMEOUT", "soon"),
            ("ASHTRAY_RATE_LIMIT_GET", "lots"),
        ]);

        assert_eq!(
            vars,
            [
                "ASHTRAY_BIND_ADDR",
                "ASHTRAY_MAX_CONNECTIONS",
                "ASHTRAY_RATE_LIMIT_GET",
                "ASHTRAY_REQUEST_TIMEOUT",
            ]
        );
    }

    #[test]
    fn origins_are_normalized_to_what_a_browser_sends() {
        let config = config(&[(
            "ASHTRAY_CORS_ALLOWED_ORIGINS",
            "https://Example.com/, http://localhost:3000",
        )])
        .expect("origins");

        let cors = config.http.cors.expect("cors on");
        assert_eq!(
            cors.origins,
            AllowedOrigins::List(vec![
                HeaderValue::from_static("https://example.com"),
                HeaderValue::from_static("http://localhost:3000"),
            ])
        );
        assert_eq!(cors.max_age, DEFAULT_CORS_MAX_AGE);
    }

    #[test]
    fn a_star_allows_any_origin() {
        let config = config(&[("ASHTRAY_CORS_ALLOWED_ORIGINS", "*")]).expect("star");

        assert_eq!(
            config.http.cors.expect("cors on").origins,
            AllowedOrigins::Any
        );
    }

    #[test]
    fn rate_limits_can_be_tuned_or_turned_off() {
        let config = config(&[
            ("ASHTRAY_RATE_LIMIT_POST", "3/10m"),
            ("ASHTRAY_RATE_LIMIT_GET", "off"),
        ])
        .expect("rates");

        assert_eq!(
            config.http.rate_limit.post,
            Some(Rate::new(3, Duration::from_secs(600)))
        );
        assert_eq!(config.http.rate_limit.get, None);
    }

    #[test]
    fn trusted_proxies_take_networks_addresses_and_keywords() {
        let config = config(&[(
            "ASHTRAY_TRUSTED_PROXIES",
            "loopback, 10.1.2.3, 172.16.0.0/12",
        )])
        .expect("proxies");

        let client_ip = config.http.client_ip;
        assert!(client_ip.trusts("127.0.0.1".parse().unwrap()));
        assert!(client_ip.trusts("::1".parse().unwrap()));
        assert!(client_ip.trusts("10.1.2.3".parse().unwrap()));
        assert!(!client_ip.trusts("10.1.2.4".parse().unwrap()));
        assert!(client_ip.trusts("172.20.0.5".parse().unwrap()));
    }

    #[test]
    fn a_default_page_larger_than_the_maximum_is_refused() {
        let vars = rejected(&[
            ("ASHTRAY_DEFAULT_PER_PAGE", "50"),
            ("ASHTRAY_MAX_PER_PAGE", "10"),
        ]);

        assert_eq!(vars, ["ASHTRAY_DEFAULT_PER_PAGE"]);
    }

    #[test]
    fn a_body_limit_below_the_largest_entry_is_a_warning() {
        let config = config(&[("ASHTRAY_MAX_BODY_SIZE", "1KiB")]).expect("valid");

        assert_eq!(config.http.max_body_bytes, 1024);
        assert_eq!(config.warnings().len(), 1);
    }

    #[test]
    fn the_log_config_rejects_an_unknown_format() {
        let vars: HashMap<&str, &str> = HashMap::from([("ASHTRAY_LOG_FORMAT", "xml")]);
        let error = LogConfig::from_lookup(&|var| vars.get(var).map(|v| (*v).to_owned()))
            .expect_err("xml is not a format");

        assert_eq!(error.0[0].var, "ASHTRAY_LOG_FORMAT");
    }
}
