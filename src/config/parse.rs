//! Parsers for single variables. Each one answers the parsed value or a reason
//! that finishes the sentence "ASHTRAY_X=\"…\": …".

use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::str::FromStr;
use std::time::Duration;

use axum::http::{HeaderName, HeaderValue};
use ipnet::IpNet;
use tracing_subscriber::EnvFilter;

use super::{AllowedOrigins, LogFormat, Rate};

/// Networks a `loopback` keyword stands for.
const LOOPBACK: &[&str] = &["127.0.0.0/8", "::1/128"];
/// Networks a `private` keyword stands for: RFC 1918 and unique local IPv6,
/// which is where Docker and Kubernetes put their proxies.
const PRIVATE: &[&str] = &["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fc00::/7"];

pub fn socket_addr(raw: &str) -> Result<SocketAddr, String> {
    raw.parse()
        .map_err(|_| "is not a socket address, e.g. `0.0.0.0:8080`".to_owned())
}

/// A whole number above zero.
pub fn positive<T>(raw: &str) -> Result<T, String>
where
    T: FromStr + PartialOrd + Default,
{
    raw.parse::<T>()
        .ok()
        .filter(|value| *value > T::default())
        .ok_or_else(|| "is not a whole number above zero".to_owned())
}

/// `10s`, `500ms`, `1m 30s` — anything `humantime` reads, as long as it is not
/// zero.
pub fn duration(raw: &str) -> Result<Duration, String> {
    let parsed = humantime::parse_duration(raw)
        .map_err(|cause| format!("is not a duration ({cause}), e.g. `10s` or `500ms`"))?;
    if parsed.is_zero() {
        return Err("has to be longer than zero".to_owned());
    }
    Ok(parsed)
}

/// A number of bytes with an optional unit: `16384`, `16KiB`, `1MiB`, `64KB`.
pub fn byte_size(raw: &str) -> Result<usize, String> {
    let digits = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (number, unit) = raw.split_at(digits);

    let number: usize = number
        .parse()
        .map_err(|_| "is not a size, e.g. `16KiB` or `16384`".to_owned())?;
    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kb" => 1_000,
        "kib" => 1 << 10,
        "mb" => 1_000_000,
        "mib" => 1 << 20,
        other => return Err(format!("has an unknown unit `{other}`; use B, KiB or MiB")),
    };

    number
        .checked_mul(multiplier)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| "has to be above zero and fit in memory".to_owned())
}

/// `<count>/<period>`, e.g. `10/1h`, `120/min`, `5/30s`; or `off`.
pub fn rate(raw: &str) -> Result<Option<Rate>, String> {
    if raw.eq_ignore_ascii_case("off") {
        return Ok(None);
    }

    let usage = "is not a rate, e.g. `10/1h`, `120/min` or `off`";
    let (count, period) = raw.split_once('/').ok_or(usage)?;
    let count: NonZeroU32 = count.trim().parse().map_err(|_| usage)?;

    // `min` alone is not a duration to humantime, `1min` is.
    let period = period.trim();
    let per = humantime::parse_duration(period)
        .or_else(|_| humantime::parse_duration(&format!("1{period}")))
        .map_err(|_| usage)?;
    if per.is_zero() {
        return Err("has a zero period".to_owned());
    }

    Ok(Some(Rate { count, per }))
}

/// `*`, or a comma-separated list of origins. Each origin is normalized to the
/// exact text a browser sends in its `Origin` header, so `https://Example.com/`
/// matches requests from `https://example.com`.
pub fn origins(raw: &str) -> Result<AllowedOrigins, String> {
    let items: Vec<&str> = list(raw).collect();

    if items == ["*"] {
        return Ok(AllowedOrigins::Any);
    }
    if items.contains(&"*") {
        return Err("mixes `*` with explicit origins; use one or the other".to_owned());
    }

    items
        .into_iter()
        .map(origin)
        .collect::<Result<_, _>>()
        .map(AllowedOrigins::List)
}

fn origin(raw: &str) -> Result<HeaderValue, String> {
    let url = url::Url::parse(raw)
        .map_err(|_| format!("`{raw}` is not an origin like `https://example.com`"))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("`{raw}` has to be http or https"));
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(format!(
            "`{raw}` has a path; an origin is only scheme, host and port"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("`{raw}` has credentials in it"));
    }

    HeaderValue::from_str(&url.origin().ascii_serialization())
        .map_err(|_| format!("`{raw}` cannot be sent as a header"))
}

/// A comma-separated list of networks (`10.0.0.0/8`), single addresses
/// (`10.0.0.7`) and the keywords `loopback` and `private`.
pub fn networks(raw: &str) -> Result<Vec<IpNet>, String> {
    let mut networks = Vec::new();

    for item in list(raw) {
        match item.to_ascii_lowercase().as_str() {
            "loopback" => networks.extend(LOOPBACK.iter().map(|net| known_network(net))),
            "private" => networks.extend(PRIVATE.iter().map(|net| known_network(net))),
            _ => {
                let network = item
                    .parse::<IpNet>()
                    .or_else(|_| item.parse::<std::net::IpAddr>().map(IpNet::from))
                    .map_err(|_| {
                        format!("`{item}` is not an address, a network like `10.0.0.0/8`, `loopback` or `private`")
                    })?;
                networks.push(network);
            }
        }
    }

    Ok(networks)
}

fn known_network(net: &str) -> IpNet {
    net.parse().expect("built-in networks are valid")
}

pub fn header_name(raw: &str) -> Result<HeaderName, String> {
    HeaderName::from_str(raw).map_err(|_| "is not a valid header name".to_owned())
}

pub fn log_filter(raw: &str) -> Result<String, String> {
    EnvFilter::try_new(raw)
        .map(|_| raw.to_owned())
        .map_err(|cause| format!("is not a tracing filter ({cause})"))
}

pub fn log_format(raw: &str) -> Result<LogFormat, String> {
    match raw.to_ascii_lowercase().as_str() {
        "text" => Ok(LogFormat::Text),
        "json" => Ok(LogFormat::Json),
        _ => Err("has to be `text` or `json`".to_owned()),
    }
}

fn list(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_take_binary_and_decimal_units() {
        assert_eq!(byte_size("16384"), Ok(16384));
        assert_eq!(byte_size("16KiB"), Ok(16 * 1024));
        assert_eq!(byte_size("16 kib"), Ok(16 * 1024));
        assert_eq!(byte_size("64KB"), Ok(64_000));
        assert_eq!(byte_size("1MiB"), Ok(1024 * 1024));
        assert!(byte_size("0").is_err());
        assert!(byte_size("16 parsecs").is_err());
        assert!(byte_size("KiB").is_err());
    }

    #[test]
    fn rates_take_a_bare_unit_or_a_duration() {
        let minute = Duration::from_secs(60);
        assert_eq!(rate("120/min"), Ok(Some(Rate::new(120, minute))));
        assert_eq!(rate("120/1m"), Ok(Some(Rate::new(120, minute))));
        assert_eq!(
            rate("5 / 30s"),
            Ok(Some(Rate::new(5, Duration::from_secs(30))))
        );
        assert_eq!(rate("10/hour"), Ok(Some(Rate::new(10, minute * 60))));
        assert_eq!(rate("OFF"), Ok(None));
        assert!(rate("0/min").is_err());
        assert!(rate("10").is_err());
        assert!(rate("10/fortnight-ish").is_err());
    }

    #[test]
    fn durations_have_to_be_positive() {
        assert_eq!(duration("1m 30s"), Ok(Duration::from_secs(90)));
        assert!(duration("0s").is_err());
        assert!(duration("10").is_err());
    }

    #[test]
    fn an_origin_with_a_path_is_refused() {
        assert!(origins("https://example.com/guestbook").is_err());
        assert!(origins("example.com").is_err());
        assert!(origins("ftp://example.com").is_err());
        assert!(origins("https://user:pass@example.com").is_err());
        assert!(origins("*, https://example.com").is_err());
    }

    #[test]
    fn default_ports_are_dropped_from_origins() {
        assert_eq!(
            origins("https://example.com:443"),
            Ok(AllowedOrigins::List(vec![HeaderValue::from_static(
                "https://example.com"
            )]))
        );
    }

    #[test]
    fn a_bare_address_is_a_single_host_network() {
        let parsed = networks("192.0.2.1, 2001:db8::1").expect("addresses");
        assert_eq!(parsed[0].to_string(), "192.0.2.1/32");
        assert_eq!(parsed[1].to_string(), "2001:db8::1/128");
        assert!(networks("192.0.2.0/33").is_err());
        assert!(networks("nginx").is_err());
    }
}
