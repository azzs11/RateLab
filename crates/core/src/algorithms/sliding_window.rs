//! Sliding window log: remembers every admitted request for one window and admits a
//! new one only if the total in the last `window` stays within `limit`.

use super::with_state;
use crate::{Clock, Cost, Decision, RateLimitConfig, RateLimiter, Tokens};
use dashmap::DashMap;
use std::collections::VecDeque;
use std::time::Duration;

#[derive(Debug, Default)]
struct Log {
    /// (admission time in ns, cost), oldest first. Zero-cost requests are not stored.
    entries: VecDeque<(u64, u64)>,
    used: u64,
}

/// Sliding window log.
///
/// Exact: in any span of length `window` it never admits more than `limit` units. The
/// price is memory: up to `limit` entries per key.
///
/// ```
/// use drl_core::{Cost, ManualClock, RateLimitConfig, RateLimiter, SlidingWindow};
/// use std::time::Duration;
///
/// let clock = ManualClock::new();
/// let limiter = SlidingWindow::with_clock(RateLimitConfig::per_second(2)?, &clock);
/// assert!(limiter.check("k", Cost::ONE).allowed);
/// clock.advance(Duration::from_millis(600));
/// assert!(limiter.check("k", Cost::ONE).allowed);
/// assert!(!limiter.check("k", Cost::ONE).allowed);
///
/// clock.advance(Duration::from_millis(400)); // the first request leaves the window
/// assert!(limiter.check("k", Cost::ONE).allowed);
/// # Ok::<(), drl_core::ConfigError>(())
/// ```
#[derive(Debug)]
pub struct SlidingWindow<C> {
    limit: u64,
    window: u64,
    clock: C,
    logs: DashMap<String, Log>,
}

impl<C: Clock> SlidingWindow<C> {
    /// A sliding window log that reads time from `clock`.
    pub fn with_clock(config: RateLimitConfig, clock: C) -> Self {
        Self {
            limit: config.limit(),
            window: config.window_nanos(),
            clock,
            logs: DashMap::new(),
        }
    }

    /// Time from `now` until an entry admitted at `at` leaves the window.
    fn expires_in(&self, at: u64, now: u64) -> Duration {
        let expiry = u128::from(at) + u128::from(self.window);
        Duration::from_nanos(
            u64::try_from(expiry.saturating_sub(u128::from(now))).unwrap_or(u64::MAX),
        )
    }
}

#[cfg(feature = "std-clock")]
impl SlidingWindow<crate::MonotonicClock> {
    /// A sliding window log on the real monotonic clock.
    pub fn new(config: RateLimitConfig) -> Self {
        Self::with_clock(config, crate::MonotonicClock::new())
    }
}

impl<C: Clock> RateLimiter for SlidingWindow<C> {
    fn check(&self, key: &str, cost: Cost) -> Decision {
        let now = self.clock.now().0;

        with_state(&self.logs, key, Log::default, |log| {
            // An entry counts while now - at < window.
            while let Some(&(at, c)) = log.entries.front() {
                if u128::from(at) + u128::from(self.window) <= u128::from(now) {
                    log.entries.pop_front();
                    log.used -= c;
                } else {
                    break;
                }
            }

            let allowed = log.used.saturating_add(cost.0) <= self.limit;
            if allowed && cost.0 > 0 {
                log.entries.push_back((now, cost.0));
                log.used += cost.0;
            }
            let remaining = Tokens(self.limit - log.used);
            let reset_after = log
                .entries
                .back()
                .map_or(Duration::ZERO, |&(at, _)| self.expires_in(at, now));

            if allowed {
                return Decision::allow(remaining, reset_after);
            }
            if cost.0 > self.limit {
                return Decision::deny(remaining, reset_after, None);
            }
            // Find the oldest entry whose expiry frees enough room.
            let mut used = log.used;
            let mut retry_after = None;
            for &(at, c) in &log.entries {
                used -= c;
                if used + cost.0 <= self.limit {
                    retry_after = Some(self.expires_in(at, now));
                    break;
                }
            }
            Decision::deny(remaining, reset_after, retry_after)
        })
    }

    fn reset(&self, key: &str) {
        self.logs.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManualClock;

    #[test]
    fn allows_limit_then_denies() {
        let clock = ManualClock::new();
        let l = SlidingWindow::with_clock(RateLimitConfig::per_second(5).unwrap(), &clock);
        for _ in 0..5 {
            assert!(l.check("user1", Cost::ONE).allowed);
        }
        assert!(!l.check("user1", Cost::ONE).allowed);
    }

    #[test]
    fn slides_as_old_requests_expire() {
        let clock = ManualClock::new();
        let config = RateLimitConfig::new(3, Duration::from_millis(500)).unwrap();
        let l = SlidingWindow::with_clock(config, &clock);
        for _ in 0..3 {
            assert!(l.check("user1", Cost::ONE).allowed);
        }
        assert!(!l.check("user1", Cost::ONE).allowed);

        clock.advance(Duration::from_millis(500));
        assert!(l.check("user1", Cost::ONE).allowed);
    }

    #[test]
    fn retry_after_waits_for_enough_expiries() {
        let clock = ManualClock::new();
        let l = SlidingWindow::with_clock(RateLimitConfig::per_second(3).unwrap(), &clock);
        assert!(l.check("k", Cost::ONE).allowed); // t = 0
        clock.advance(Duration::from_millis(100));
        assert!(l.check("k", Cost::ONE).allowed); // t = 100
        clock.advance(Duration::from_millis(100));
        assert!(l.check("k", Cost::ONE).allowed); // t = 200

        // Two units need the first two entries gone: at t = 1100, i.e. in 900 ms.
        let d = l.check("k", Cost(2));
        assert!(!d.allowed);
        assert_eq!(d.retry_after, Some(Duration::from_millis(900)));
        assert_eq!(d.reset_after, Duration::from_millis(1000));
    }

    #[test]
    fn no_boundary_burst() {
        let clock = ManualClock::starting_at(crate::Nanos(999_000_000));
        let l = SlidingWindow::with_clock(RateLimitConfig::per_second(10).unwrap(), &clock);
        assert!(l.check("k", Cost(10)).allowed);
        clock.advance(Duration::from_millis(2));
        assert!(!l.check("k", Cost::ONE).allowed);
    }
}
