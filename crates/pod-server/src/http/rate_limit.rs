//! In-memory rate limiting for login and register (spec 4.1: 10 per minute
//! per IP and per username).
//!
//! Each key keeps the instants of its admitted attempts inside the window, at
//! most `limit` of them, so the limit is exact (a sliding log, not fixed
//! windows that allow a double burst across a boundary). The state lives in
//! one process; a second server instance would count separately.
// TODO: move the counters to Postgres or a shared store if the server ever
// runs more than one instance.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::auth::Username;

/// What an attempt is counted against.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RateKey {
    /// Built with [`RateKey::ip`], which groups IPv6 addresses by /64.
    Ip(IpAddr),
    Username(Username),
}

impl RateKey {
    /// An IPv6 client usually owns a whole /64, so counting single addresses
    /// would let it rotate through 2^64 of them. IPv4 addresses count alone.
    pub fn ip(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(_) => Self::Ip(ip),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self::Ip(IpAddr::V4(v4)),
                None => {
                    let prefix = u128::from(v6) & !((1u128 << 64) - 1);
                    Self::Ip(IpAddr::V6(prefix.into()))
                }
            },
        }
    }
}

/// How long the caller must wait before the next attempt can be admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryAfter(pub Duration);

impl RetryAfter {
    /// Whole seconds for the `Retry-After` header, rounded up so a client
    /// that waits exactly this long is admitted.
    pub fn as_secs_ceil(self) -> u64 {
        self.0.as_secs() + u64::from(self.0.subsec_nanos() > 0)
    }
}

#[derive(Debug)]
pub struct RateLimiter {
    limit: usize,
    window: Duration,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    /// Admitted attempts per key, oldest first, never more than `limit`.
    hits: HashMap<RateKey, VecDeque<Instant>>,
    last_prune: Option<Instant>,
}

impl RateLimiter {
    /// The spec's limit for login and register.
    pub fn auth() -> Self {
        Self::new(10, Duration::from_secs(60))
    }

    pub fn new(limit: usize, window: Duration) -> Self {
        assert!(limit > 0, "a zero limit would reject every attempt");
        Self {
            limit,
            window,
            state: Mutex::new(State {
                hits: HashMap::new(),
                last_prune: None,
            }),
        }
    }

    /// Admits the attempt only if every key is under the limit, and then
    /// counts it against all of them. A rejected attempt counts against
    /// none, so a client hammering a limited username doesn't also use up
    /// its IP budget.
    pub fn check(&self, keys: &[RateKey], now: Instant) -> Result<(), RetryAfter> {
        let mut state = self
            .state
            .lock()
            .expect("no code panics while holding the rate limit lock");
        state.prune(now, self.window);

        // The sweep runs once per window, so these keys may still hold hits
        // that just left it.
        for key in keys {
            if let Some(hits) = state.hits.get_mut(key) {
                drop_expired(hits, now, self.window);
            }
        }

        let wait = keys
            .iter()
            .filter_map(|key| {
                let hits = state.hits.get(key)?;
                (hits.len() >= self.limit).then(|| {
                    let oldest = *hits.front().expect("a full log is not empty");
                    (oldest + self.window).saturating_duration_since(now)
                })
            })
            .max();
        if let Some(wait) = wait {
            return Err(RetryAfter(wait));
        }

        for key in keys {
            let hits = state.hits.entry(key.clone()).or_default();
            hits.push_back(now);
            if hits.len() > self.limit {
                hits.pop_front();
            }
        }
        Ok(())
    }
}

impl State {
    /// Drops attempts older than the window. Every key is swept at most once
    /// per window, which keeps memory proportional to the attempts of the
    /// last two windows while keeping `check` amortized O(keys).
    fn prune(&mut self, now: Instant, window: Duration) {
        if self
            .last_prune
            .is_none_or(|at| now.saturating_duration_since(at) >= window)
        {
            self.hits.retain(|_, hits| {
                drop_expired(hits, now, window);
                !hits.is_empty()
            });
            self.last_prune = Some(now);
        }
    }
}

fn drop_expired(hits: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while hits
        .front()
        .is_some_and(|&at| now.saturating_duration_since(at) >= window)
    {
        hits.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(addr: &str) -> RateKey {
        RateKey::ip(addr.parse().expect("valid IP literal"))
    }

    #[test]
    fn admits_up_to_the_limit_then_reports_the_wait() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let start = Instant::now();
        let key = [ip("192.0.2.1")];
        for second in 0..3 {
            limiter
                .check(&key, start + Duration::from_secs(second))
                .expect("under the limit");
        }
        let error = limiter
            .check(&key, start + Duration::from_millis(10_500))
            .expect_err("the fourth attempt is over the limit");
        assert_eq!(error, RetryAfter(Duration::from_millis(49_500)));
        assert_eq!(error.as_secs_ceil(), 50);
    }

    #[test]
    fn window_slides() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        let start = Instant::now();
        let key = [ip("192.0.2.1")];
        limiter.check(&key, start).expect("first");
        limiter
            .check(&key, start + Duration::from_secs(30))
            .expect("second");
        limiter
            .check(&key, start + Duration::from_secs(59))
            .expect_err("both hits are inside the window");
        limiter
            .check(&key, start + Duration::from_secs(60))
            .expect("the first hit left the window");
        limiter
            .check(&key, start + Duration::from_secs(61))
            .expect_err("the hits at 30 s and 60 s are inside the window");
    }

    #[test]
    fn hits_leave_the_window_between_sweeps() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let start = Instant::now();
        limiter
            .check(&[ip("192.0.2.1")], start)
            .expect("sweeps at 0 s");
        limiter
            .check(&[ip("192.0.2.2")], start + Duration::from_secs(1))
            .expect("fresh key");
        limiter
            .check(&[ip("192.0.2.3")], start + Duration::from_secs(60))
            .expect("sweeps at 60 s, while the hit at 1 s is still inside the window");
        limiter
            .check(&[ip("192.0.2.2")], start + Duration::from_secs(61))
            .expect("the hit at 1 s left the window although no sweep ran since 60 s");
    }

    #[test]
    fn every_key_must_be_under_the_limit() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        let alice = RateKey::Username(Username::parse("alice").expect("valid username"));
        limiter
            .check(&[ip("192.0.2.1"), alice.clone()], now)
            .expect("first attempt");
        limiter
            .check(&[ip("192.0.2.2"), alice.clone()], now)
            .expect_err("the username is used up");
        limiter
            .check(
                &[
                    ip("192.0.2.2"),
                    RateKey::Username(Username::parse("bob").expect("valid username")),
                ],
                now,
            )
            .expect("the rejected attempt did not count against 192.0.2.2");
    }

    #[test]
    fn ipv6_counts_per_slash_64() {
        assert_eq!(ip("2001:db8:1:2:aaaa::1"), ip("2001:db8:1:2:bbbb::2"));
        assert_ne!(ip("2001:db8:1:2::1"), ip("2001:db8:1:3::1"));
        assert_eq!(ip("::ffff:192.0.2.1"), ip("192.0.2.1"));
    }

    #[test]
    fn prune_forgets_idle_keys() {
        let limiter = RateLimiter::new(5, Duration::from_secs(60));
        let start = Instant::now();
        for host in 0..100u8 {
            limiter
                .check(&[ip(&format!("192.0.2.{host}"))], start)
                .expect("fresh key");
        }
        limiter
            .check(&[ip("198.51.100.1")], start + Duration::from_secs(60))
            .expect("fresh key");
        let state = limiter.state.lock().expect("not poisoned");
        assert_eq!(state.hits.len(), 1);
    }
}
