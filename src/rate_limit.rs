//! Per-client request budgets, kept in process memory.
//!
//! One budget per method: writing an entry is what spammers are after, so
//! `POST` gets a tight one, while reading gets a generous one that only stops
//! scrapers. The state lives in this process alone — two replicas each keep
//! their own count, and a restart forgives everyone.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use governor::clock::Clock;
use governor::{DefaultKeyedRateLimiter, Quota};

use crate::client_ip::ClientIp;
use crate::config::{Rate, RateLimitConfig};
use crate::error::ApiError;
use crate::metrics::Metrics;

/// How many checks pass between two sweeps of forgotten clients. The sweep
/// walks the whole table, so it runs once per this many requests rather than
/// on every one; a table only grows as fast as requests arrive, so this keeps
/// it bounded without a background task.
const SWEEP_EVERY: u64 = 4096;

/// IPv6 clients are counted per /64: that is what one subscriber usually
/// gets, and counting single addresses would let them rotate through
/// billions of them.
const IPV6_PREFIX: u32 = 64;

#[derive(Clone)]
pub struct RateLimits {
    post: Option<Arc<Limiter>>,
    get: Option<Arc<Limiter>>,
    metrics: Arc<Metrics>,
}

impl RateLimits {
    pub fn new(config: &RateLimitConfig, metrics: Arc<Metrics>) -> Self {
        Self {
            post: config.post.map(Limiter::new).map(Arc::new),
            get: config.get.map(Limiter::new).map(Arc::new),
            metrics,
        }
    }

    fn for_method(&self, method: &Method) -> Option<&Limiter> {
        match *method {
            Method::POST => self.post.as_deref(),
            Method::GET | Method::HEAD => self.get.as_deref(),
            _ => None,
        }
    }
}

struct Limiter {
    inner: DefaultKeyedRateLimiter<IpAddr>,
    checks: AtomicU64,
}

impl Limiter {
    fn new(rate: Rate) -> Self {
        let quota = Quota::with_period(rate.per / rate.count.get())
            .expect("a rate has a non-zero period")
            .allow_burst(rate.count);
        Self {
            inner: DefaultKeyedRateLimiter::keyed(quota),
            checks: AtomicU64::new(0),
        }
    }

    /// Spends one request from `client`'s budget, or answers how long until
    /// there is one to spend.
    fn check(&self, client: IpAddr) -> Result<(), ApiError> {
        if self.checks.fetch_add(1, Ordering::Relaxed) % SWEEP_EVERY == SWEEP_EVERY - 1 {
            self.inner.retain_recent();
            self.inner.shrink_to_fit();
        }

        self.inner
            .check_key(&bucket(client))
            .map_err(|not_until| ApiError::RateLimited {
                retry_after: not_until.wait_time_from(self.inner.clock().now()),
            })
    }
}

/// Lets the request through if the client still has budget for its method.
pub async fn enforce(State(limits): State<RateLimits>, request: Request, next: Next) -> Response {
    let Some(limiter) = limits.for_method(request.method()) else {
        return next.run(request).await;
    };

    let client = request
        .extensions()
        .get::<ClientIp>()
        .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |ip| ip.0);

    match limiter.check(client) {
        Ok(()) => next.run(request).await,
        Err(refusal) => {
            tracing::info!(client = %client, method = %request.method(), "rate limited");
            limits.metrics.rate_limited(request.method());
            refusal.into_response()
        }
    }
}

fn bucket(client: IpAddr) -> IpAddr {
    match client {
        IpAddr::V4(_) => client,
        IpAddr::V6(v6) => {
            let mask = u128::MAX << (128 - IPV6_PREFIX);
            IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & mask))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_client_gets_its_burst_and_then_waits() {
        let limiter = Limiter::new(Rate::new(2, Duration::from_secs(60)));
        let client: IpAddr = "198.51.100.7".parse().unwrap();

        assert!(limiter.check(client).is_ok());
        assert!(limiter.check(client).is_ok());

        let Err(ApiError::RateLimited { retry_after }) = limiter.check(client) else {
            panic!("the third request should be refused");
        };
        // One request comes back every 30 seconds.
        assert!(retry_after <= Duration::from_secs(30));
        assert!(retry_after > Duration::from_secs(29));
    }

    #[test]
    fn clients_have_budgets_of_their_own() {
        let limiter = Limiter::new(Rate::new(1, Duration::from_secs(60)));

        assert!(limiter.check("198.51.100.7".parse().unwrap()).is_ok());
        assert!(limiter.check("198.51.100.8".parse().unwrap()).is_ok());
    }

    #[test]
    fn an_ipv6_subnet_shares_one_budget() {
        let limiter = Limiter::new(Rate::new(1, Duration::from_secs(60)));

        assert!(limiter.check("2001:db8:0:1::1".parse().unwrap()).is_ok());
        assert!(
            limiter
                .check("2001:db8:0:1::ffff".parse().unwrap())
                .is_err()
        );
        assert!(limiter.check("2001:db8:0:2::1".parse().unwrap()).is_ok());
    }
}
