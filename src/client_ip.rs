//! Who is on the other end of a request.
//!
//! Without a proxy that is simply the TCP peer. Behind one, the peer is the
//! proxy and the client is in a header — but anyone can send that header, so
//! it is only read when the peer is in `ASHTRAY_TRUSTED_PROXIES`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::http::header::FORWARDED;
use axum::middleware::Next;
use axum::response::Response;

use crate::config::ClientIpConfig;

/// The address a request is attributed to, for the log and the rate limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpAddr);

/// Resolves the client once per request and leaves it in the extensions for
/// every layer and handler after this one.
pub async fn resolve(
    State(config): State<Arc<ClientIpConfig>>,
    mut request: Request,
    next: Next,
) -> Response {
    // The listener always attaches the peer. A router driven without one, as
    // in the tests, gets an address that every such request shares.
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |info| info.0.ip());

    let client = client_ip(peer, request.headers(), &config);
    request.extensions_mut().insert(ClientIp(client));

    next.run(request).await
}

/// The client behind `peer`, according to the forwarding header when — and
/// only when — `peer` is a trusted proxy.
pub fn client_ip(peer: IpAddr, headers: &HeaderMap, config: &ClientIpConfig) -> IpAddr {
    let peer = peer.to_canonical();
    if !config.trusts(peer) {
        return peer;
    }

    let values = headers
        .get_all(&config.header)
        .iter()
        .filter_map(|value| value.to_str().ok());

    let hops: Vec<&str> = if config.header == FORWARDED {
        values.flat_map(forwarded_for).collect()
    } else {
        values.flat_map(|value| value.split(',')).collect()
    };

    // Walk from the proxy closest to us outwards. Every hop up to the first
    // untrusted one was appended by a proxy we trust; anything to the left of
    // that hop was written by the client and can say whatever it likes.
    let mut client = peer;
    for hop in hops.iter().rev() {
        let Some(hop) = parse_hop(hop) else {
            // A hop that is not an address breaks the chain: nothing past it
            // can be vouched for, so stay with the last one that could.
            break;
        };
        client = hop;
        if !config.trusts(hop) {
            break;
        }
    }
    client
}

/// The `for=` parameters of an RFC 7239 `Forwarded` header.
fn forwarded_for(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').filter_map(|element| {
        element.split(';').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            key.trim()
                .eq_ignore_ascii_case("for")
                .then(|| value.trim().trim_matches('"'))
        })
    })
}

/// An address as proxies write it: bare (`192.0.2.1`, `2001:db8::1`), with a
/// port (`192.0.2.1:4711`), or bracketed (`[2001:db8::1]:4711`).
fn parse_hop(hop: &str) -> Option<IpAddr> {
    let hop = hop.trim();
    hop.parse::<IpAddr>()
        .or_else(|_| hop.parse::<SocketAddr>().map(|addr| addr.ip()))
        .or_else(|_| hop.trim_start_matches('[').trim_end_matches(']').parse())
        .ok()
        .map(|ip: IpAddr| ip.to_canonical())
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderName, HeaderValue};

    use super::*;

    const PROXY: &str = "10.0.0.2";

    fn config(header: &'static str) -> ClientIpConfig {
        ClientIpConfig {
            trusted_proxies: vec!["10.0.0.0/8".parse().unwrap()],
            header: HeaderName::from_static(header),
        }
    }

    fn headers(name: &'static str, values: &[&'static str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(name, HeaderValue::from_static(value));
        }
        headers
    }

    fn ip(raw: &str) -> IpAddr {
        raw.parse().unwrap()
    }

    #[test]
    fn an_untrusted_peer_is_the_client_whatever_it_claims() {
        let headers = headers("x-forwarded-for", &["198.51.100.7"]);

        let client = client_ip(ip("203.0.113.9"), &headers, &config("x-forwarded-for"));

        assert_eq!(client, ip("203.0.113.9"));
    }

    #[test]
    fn a_trusted_proxy_is_believed() {
        let headers = headers("x-forwarded-for", &["198.51.100.7"]);

        let client = client_ip(ip(PROXY), &headers, &config("x-forwarded-for"));

        assert_eq!(client, ip("198.51.100.7"));
    }

    #[test]
    fn a_spoofed_hop_to_the_left_of_the_real_client_is_ignored() {
        // The client sent `X-Forwarded-For: 1.1.1.1`; the proxy appended the
        // address it actually saw.
        let headers = headers("x-forwarded-for", &["1.1.1.1, 198.51.100.7"]);

        let client = client_ip(ip(PROXY), &headers, &config("x-forwarded-for"));

        assert_eq!(client, ip("198.51.100.7"));
    }

    #[test]
    fn a_chain_of_trusted_proxies_is_walked_through() {
        let headers = headers("x-forwarded-for", &["198.51.100.7, 10.0.0.5", "10.0.0.3"]);

        let client = client_ip(ip(PROXY), &headers, &config("x-forwarded-for"));

        assert_eq!(client, ip("198.51.100.7"));
    }

    #[test]
    fn garbage_in_the_header_falls_back_to_the_last_hop_we_trust() {
        let headers = headers("x-forwarded-for", &["198.51.100.7, unknown"]);

        let client = client_ip(ip(PROXY), &headers, &config("x-forwarded-for"));

        assert_eq!(client, ip(PROXY));
    }

    #[test]
    fn a_missing_header_leaves_the_proxy_as_the_client() {
        let client = client_ip(ip(PROXY), &HeaderMap::new(), &config("x-forwarded-for"));

        assert_eq!(client, ip(PROXY));
    }

    #[test]
    fn a_single_value_header_works_too() {
        let headers = headers("x-real-ip", &["2001:db8::7"]);

        let client = client_ip(ip(PROXY), &headers, &config("x-real-ip"));

        assert_eq!(client, ip("2001:db8::7"));
    }

    #[test]
    fn the_standard_forwarded_header_is_understood() {
        let headers = headers(
            "forwarded",
            &[r#"for=198.51.100.7;proto=https, for="[2001:db8::1]:4711""#],
        );
        let mut config = config("forwarded");
        config
            .trusted_proxies
            .push("2001:db8::1/128".parse().unwrap());

        let client = client_ip(ip(PROXY), &headers, &config);

        assert_eq!(client, ip("198.51.100.7"));
    }

    #[test]
    fn ipv4_mapped_addresses_are_unwrapped() {
        let client = client_ip(
            ip("::ffff:10.0.0.2"),
            &headers("x-forwarded-for", &["::ffff:198.51.100.7"]),
            &config("x-forwarded-for"),
        );

        assert_eq!(client, ip("198.51.100.7"));
    }
}
