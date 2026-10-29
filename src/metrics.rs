//! Prometheus metrics, served on a listener of their own.
//!
//! The classic set: request rate, errors and latency per route
//! (`http_requests_total`, `http_request_duration_seconds`,
//! `http_requests_in_flight`), the `process_*` family a Prometheus client is
//! expected to export, and what is particular to a guestbook — entries written
//! and stored, requests refused by the rate limit, and the state of the
//! database pool.

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use prometheus_client::collector::Collector;
use prometheus_client::encoding::text::encode;
use prometheus_client::encoding::{DescriptorEncoder, EncodeLabelSet, EncodeMetric};
use prometheus_client::metrics::counter::{ConstCounter, Counter};
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::{ConstGauge, Gauge};
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::metrics::info::Info;
use prometheus_client::registry::{Registry, Unit};
use sqlx::SqlitePool;

use crate::AppState;
use crate::error::ApiError;

const CONTENT_TYPE: &str = "application/openmetrics-text; version=1.0.0; charset=utf-8";

/// The classic Prometheus latency buckets, in seconds.
const LATENCY_BUCKETS: [f64; 11] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// The route label for a request no route matched. Using the raw path instead
/// would let every scanner probing `/wp-login.php` mint a new time series.
const UNMATCHED: &str = "unmatched";

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct RequestLabels {
    method: &'static str,
    route: String,
    status: u16,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct RouteLabels {
    method: &'static str,
    route: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct MethodLabels {
    method: &'static str,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct BuildLabels {
    version: &'static str,
}

type LatencyFamily = Family<RouteLabels, Histogram, fn() -> Histogram>;

/// Every metric the service exports, and the registry that encodes them.
#[derive(Debug)]
pub struct Metrics {
    registry: Registry,
    requests: Family<RequestLabels, Counter>,
    latency: LatencyFamily,
    in_flight: Gauge,
    entries_created: Counter,
    entries_stored: Gauge,
    rate_limited: Family<MethodLabels, Counter>,
    database_up: Gauge,
}

impl Metrics {
    pub fn new(pool: &SqlitePool) -> Self {
        let mut registry = Registry::default();

        let requests = Family::<RequestLabels, Counter>::default();
        registry.register(
            "http_requests",
            "HTTP requests answered, by method, route and status",
            requests.clone(),
        );

        let latency: LatencyFamily =
            Family::new_with_constructor(|| Histogram::new(LATENCY_BUCKETS));
        registry.register_with_unit(
            "http_request_duration",
            "Time from receiving a request to answering it, by method and route",
            Unit::Seconds,
            latency.clone(),
        );

        let in_flight = Gauge::default();
        registry.register(
            "http_requests_in_flight",
            "HTTP requests being answered right now",
            in_flight.clone(),
        );

        let entries_created = Counter::default();
        registry.register(
            "ashtray_entries_created",
            "Entries written since the process started",
            entries_created.clone(),
        );

        let entries_stored = Gauge::default();
        registry.register(
            "ashtray_entries",
            "Entries in the database, counted at scrape time",
            entries_stored.clone(),
        );

        let rate_limited = Family::<MethodLabels, Counter>::default();
        registry.register(
            "ashtray_rate_limited_requests",
            "Requests refused by the rate limit, by method",
            rate_limited.clone(),
        );

        let database_up = Gauge::default();
        registry.register(
            "ashtray_database_up",
            "1 if the database answered the last scrape, 0 if it did not",
            database_up.clone(),
        );

        registry.register(
            "ashtray_build",
            "The running version",
            Info::new(BuildLabels {
                version: env!("CARGO_PKG_VERSION"),
            }),
        );

        registry.register_collector(Box::new(PoolCollector(pool.clone())));
        registry.register_collector(Box::new(ProcessCollector::new()));

        Self {
            registry,
            requests,
            latency,
            in_flight,
            entries_created,
            entries_stored,
            rate_limited,
            database_up,
        }
    }

    pub fn entry_created(&self) {
        self.entries_created.inc();
    }

    pub fn rate_limited(&self, method: &Method) {
        self.rate_limited
            .get_or_create(&MethodLabels {
                method: method_label(method),
            })
            .inc();
    }

    /// The registry in the OpenMetrics text format.
    pub fn render(&self) -> Result<String, std::fmt::Error> {
        let mut body = String::new();
        encode(&mut body, &self.registry)?;
        Ok(body)
    }
}

/// Counts and times every request that reaches a route — or fails to.
pub async fn track(State(metrics): State<Arc<Metrics>>, request: Request, next: Next) -> Response {
    let method = method_label(request.method());
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or(UNMATCHED, MatchedPath::as_str)
        .to_owned();

    let _in_flight = InFlight::enter(&metrics.in_flight);
    let started = Instant::now();
    let response = next.run(request).await;

    metrics
        .latency
        .get_or_create(&RouteLabels {
            method,
            route: route.clone(),
        })
        .observe(started.elapsed().as_secs_f64());
    metrics
        .requests
        .get_or_create(&RequestLabels {
            method,
            route,
            status: response.status().as_u16(),
        })
        .inc();

    response
}

/// Keeps `http_requests_in_flight` honest when a client hangs up and the
/// request future is dropped halfway.
struct InFlight<'a>(&'a Gauge);

impl<'a> InFlight<'a> {
    fn enter(gauge: &'a Gauge) -> Self {
        gauge.inc();
        Self(gauge)
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.dec();
    }
}

/// Methods outside the ones the API knows collapse into one label value: the
/// method is client-supplied text, and every distinct value is a new series.
fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::HEAD => "HEAD",
        Method::POST => "POST",
        Method::OPTIONS => "OPTIONS",
        Method::PUT => "PUT",
        Method::PATCH => "PATCH",
        Method::DELETE => "DELETE",
        _ => "other",
    }
}

/// The router for the metrics listener: `GET /metrics`, behind a bearer
/// token when one is configured.
pub fn router(state: AppState, token: Option<Arc<str>>) -> Router {
    let mut router = Router::new().route("/metrics", get(scrape));
    if let Some(token) = token {
        router = router.layer(axum::middleware::from_fn_with_state(token, authorize));
    }
    router
        .layer(axum::middleware::from_fn(crate::middleware::json_errors))
        .with_state(state)
}

async fn scrape(State(state): State<AppState>) -> Response {
    match sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM entries")
        .fetch_one(&state.pool)
        .await
    {
        Ok(count) => {
            state.metrics.entries_stored.set(count);
            state.metrics.database_up.set(1);
        }
        Err(cause) => {
            tracing::warn!(%cause, "cannot count entries for the metrics");
            state.metrics.database_up.set(0);
        }
    }

    match state.metrics.render() {
        Ok(body) => ([(header::CONTENT_TYPE, CONTENT_TYPE)], body).into_response(),
        Err(_) => ApiError::Status(StatusCode::INTERNAL_SERVER_ERROR).into_response(),
    }
}

async fn authorize(State(token): State<Arc<str>>, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    match presented {
        Some(presented) if constant_time_eq(presented.as_bytes(), token.as_bytes()) => {
            next.run(request).await
        }
        _ => {
            let mut response = ApiError::Status(StatusCode::UNAUTHORIZED).into_response();
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            response
        }
    }
}

/// Compares without stopping at the first difference, so the time taken does
/// not tell an attacker how much of the token they already have right.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// The connection pool, read at scrape time.
#[derive(Debug)]
struct PoolCollector(SqlitePool);

impl Collector for PoolCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let size = i64::from(self.0.size());
        let idle = i64::try_from(self.0.num_idle()).unwrap_or(i64::MAX);

        let open = ConstGauge::new(size);
        open.encode(encoder.encode_descriptor(
            "ashtray_db_connections",
            "Connections the pool holds open",
            None,
            open.metric_type(),
        )?)?;

        let idle = ConstGauge::new(idle);
        idle.encode(encoder.encode_descriptor(
            "ashtray_db_connections_idle",
            "Open connections not in use right now",
            None,
            idle.metric_type(),
        )?)?;

        Ok(())
    }
}

/// The standard `process_*` metrics. The start time is everywhere; the rest
/// comes from `/proc` and is only there on Linux, which is where the image
/// runs.
#[derive(Debug)]
struct ProcessCollector {
    start_time: f64,
}

impl ProcessCollector {
    fn new() -> Self {
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |since| since.as_secs_f64());
        Self { start_time }
    }
}

impl Collector for ProcessCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let start = ConstGauge::new(self.start_time);
        start.encode(encoder.encode_descriptor(
            "process_start_time",
            "Start time of the process since the Unix epoch",
            Some(&Unit::Seconds),
            start.metric_type(),
        )?)?;

        #[cfg(target_os = "linux")]
        if let Some(sample) = proc::sample() {
            sample.encode(&mut encoder)?;
        }

        Ok(())
    }
}

/// One reading of `/proc/self`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Default, PartialEq)]
struct ProcessSample {
    cpu_seconds: f64,
    virtual_bytes: u64,
    resident_bytes: u64,
    threads: u64,
    open_fds: u64,
    max_fds: u64,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl ProcessSample {
    fn encode(&self, encoder: &mut DescriptorEncoder) -> Result<(), std::fmt::Error> {
        let cpu = ConstCounter::new(self.cpu_seconds);
        cpu.encode(encoder.encode_descriptor(
            "process_cpu",
            "User and system CPU time spent",
            Some(&Unit::Seconds),
            cpu.metric_type(),
        )?)?;

        let gauges: [(&str, &str, Option<&Unit>, u64); 5] = [
            (
                "process_virtual_memory",
                "Virtual memory size",
                Some(&Unit::Bytes),
                self.virtual_bytes,
            ),
            (
                "process_resident_memory",
                "Resident memory size",
                Some(&Unit::Bytes),
                self.resident_bytes,
            ),
            ("process_threads", "OS threads", None, self.threads),
            (
                "process_open_fds",
                "Open file descriptors",
                None,
                self.open_fds,
            ),
            (
                "process_max_fds",
                "Limit on open file descriptors",
                None,
                self.max_fds,
            ),
        ];
        for (name, help, unit, value) in gauges {
            let gauge = ConstGauge::new(i64::try_from(value).unwrap_or(i64::MAX));
            gauge.encode(encoder.encode_descriptor(name, help, unit, gauge.metric_type())?)?;
        }

        Ok(())
    }
}

/// Parsers for the `/proc` files, kept free of any I/O so they are tested on
/// every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod proc {
    /// `/proc` reports CPU time in clock ticks of `USER_HZ`, which the kernel
    /// ABI fixes at 100 on every architecture Linux ships for.
    const TICKS_PER_SECOND: f64 = 100.0;

    #[cfg(target_os = "linux")]
    pub(super) fn sample() -> Option<super::ProcessSample> {
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let limits = std::fs::read_to_string("/proc/self/limits").ok()?;
        let open_fds = std::fs::read_dir("/proc/self/fd").ok()?.count();

        let (cpu_seconds, virtual_bytes) = stat_fields(&stat)?;
        Some(super::ProcessSample {
            cpu_seconds,
            virtual_bytes,
            resident_bytes: status_value(&status, "VmRSS")? * 1024,
            threads: status_value(&status, "Threads")?,
            open_fds: u64::try_from(open_fds).ok()?,
            max_fds: max_open_files(&limits)?,
        })
    }

    /// CPU seconds and virtual memory from `/proc/self/stat`. The command name
    /// in parentheses can hold spaces and parentheses itself, so fields are
    /// counted from the last `)`.
    pub(super) fn stat_fields(stat: &str) -> Option<(f64, u64)> {
        let after_name = &stat[stat.rfind(')')? + 1..];
        let fields: Vec<&str> = after_name.split_whitespace().collect();
        // Field numbers from proc(5), minus the two before the name's end.
        let field = |number: usize| fields.get(number - 3)?.parse::<u64>().ok();

        let ticks = field(14)? + field(15)?;
        #[allow(clippy::cast_precision_loss)]
        let cpu_seconds = ticks as f64 / TICKS_PER_SECOND;
        Some((cpu_seconds, field(23)?))
    }

    /// The number on a line of `/proc/self/status`; sizes there are in KiB.
    pub(super) fn status_value(status: &str, key: &str) -> Option<u64> {
        status.lines().find_map(|line| {
            let (name, rest) = line.split_once(':')?;
            (name == key).then(|| rest.split_whitespace().next()?.parse().ok())?
        })
    }

    /// The soft limit on open files from `/proc/self/limits`.
    pub(super) fn max_open_files(limits: &str) -> Option<u64> {
        let line = limits
            .lines()
            .find(|line| line.starts_with("Max open files"))?;
        line["Max open files".len()..]
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stat_is_read_past_a_name_with_spaces_and_parentheses() {
            let stat = "4242 (ash (tray) x) S 1 4242 4242 0 -1 4194560 1003 0 0 0 \
                        150 50 0 0 20 0 9 0 12345 104857600 2048 18446744073709551615";

            assert_eq!(stat_fields(stat), Some((2.0, 104_857_600)));
        }

        #[test]
        fn status_lines_are_found_by_key() {
            let status = "Name:\tashtray\nVmRSS:\t   10240 kB\nThreads:\t9\n";

            assert_eq!(status_value(status, "VmRSS"), Some(10_240));
            assert_eq!(status_value(status, "Threads"), Some(9));
            assert_eq!(status_value(status, "VmSwap"), None);
        }

        #[test]
        fn the_soft_limit_on_open_files_is_read() {
            let limits = "Limit                     Soft Limit           Hard Limit           Units\n\
                          Max processes             63432                63432                processes\n\
                          Max open files            1048576              1048576              files\n";

            assert_eq!(max_open_files(limits), Some(1_048_576));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_compare_whole() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret-and-more"));
    }

    #[test]
    fn unknown_methods_share_one_label() {
        assert_eq!(method_label(&Method::GET), "GET");
        assert_eq!(method_label(&Method::from_bytes(b"BREW").unwrap()), "other");
    }
}
