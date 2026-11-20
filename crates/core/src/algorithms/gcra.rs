//! GCRA (Generic Cell Rate Algorithm): tracks one "theoretical arrival time" (TAT)
//! per key. Each unit pushes the TAT forward by one emission interval
//! (`window / limit`); a request is admitted if the TAT would stay within one window
//! of now.

use super::{ceil_div_nanos, sat_u64, with_state};
use crate::{Clock, Cost, Decision, RateLimitConfig, RateLimiter, Tokens};
use dashmap::DashMap;

/// Generic Cell Rate Algorithm.
///
/// Admits exactly what a [`TokenBucket`](crate::TokenBucket) with the same config
/// admits (a burst of `limit`, then `limit` per `window`), but stores a single number
/// per key instead of a level and a timestamp.
///
/// ```
/// use drl_core::{Cost, Gcra, ManualClock, RateLimitConfig, RateLimiter};
/// use std::time::Duration;
///
/// let clock = ManualClock::new();
/// let limiter = Gcra::with_clock(RateLimitConfig::per_second(4)?, &clock);
/// assert!(limiter.check("k", Cost(4)).allowed);
/// let denied = limiter.check("k", Cost::ONE);
/// assert_eq!(denied.retry_after, Some(Duration::from_millis(250)));
///
/// clock.advance(Duration::from_millis(250));
/// assert!(limiter.check("k", Cost::ONE).allowed);
/// # Ok::<(), drl_core::ConfigError>(())
/// ```
#[derive(Debug)]
pub struct Gcra<C> {
    limit: u128,
    window: u128,
    clock: C,
    /// Theoretical arrival time per key, in nanoseconds scaled by `limit`, so the
    /// emission interval (`window / limit` ns) is the exact integer `window`.
    tats: DashMap<String, u128>,
}

impl<C: Clock> Gcra<C> {
    /// A GCRA limiter that reads time from `clock`.
    pub fn with_clock(config: RateLimitConfig, clock: C) -> Self {
        Self {
            limit: u128::from(config.limit()),
            window: u128::from(config.window_nanos()),
            clock,
            tats: DashMap::new(),
        }
    }
}

#[cfg(feature = "std-clock")]
impl Gcra<crate::MonotonicClock> {
    /// A GCRA limiter on the real monotonic clock.
    pub fn new(config: RateLimitConfig) -> Self {
        Self::with_clock(config, crate::MonotonicClock::new())
    }
}

impl<C: Clock> RateLimiter for Gcra<C> {
    fn check(&self, key: &str, cost: Cost) -> Decision {
        let now = u128::from(self.clock.now().0) * self.limit;
        // Burst tolerance: `limit` emission intervals, i.e. one window (scaled).
        let tolerance = self.window * self.limit;
        let increment = u128::from(cost.0) * self.window;

        with_state(
            &self.tats,
            key,
            || now,
            |tat| {
                let base = (*tat).max(now);
                let new_tat = base.saturating_add(increment);
                let allowed = new_tat - now <= tolerance;
                if allowed {
                    *tat = new_tat;
                }

                let backlog = (*tat).max(now) - now;
                let remaining = Tokens(sat_u64((tolerance - backlog) / self.window));
                let reset_after = ceil_div_nanos(backlog, self.limit);

                if allowed {
                    Decision::allow(remaining, reset_after)
                } else {
                    let retry_after = (u128::from(cost.0) <= self.limit)
                        .then(|| ceil_div_nanos(new_tat - now - tolerance, self.limit));
                    Decision::deny(remaining, reset_after, retry_after)
                }
            },
        )
    }

    fn reset(&self, key: &str) {
        self.tats.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Algorithm, ManualClock, TokenBucket};
    use std::time::Duration;

    #[test]
    fn allows_burst_then_one_per_interval() {
        let clock = ManualClock::new();
        let l = Gcra::with_clock(RateLimitConfig::per_second(10).unwrap(), &clock);
        for _ in 0..10 {
            assert!(l.check("k", Cost::ONE).allowed);
        }
        assert!(!l.check("k", Cost::ONE).allowed);
        clock.advance(Duration::from_millis(99));
        assert!(!l.check("k", Cost::ONE).allowed);
        clock.advance(Duration::from_millis(1));
        assert!(l.check("k", Cost::ONE).allowed);
    }

    #[test]
    fn matches_token_bucket_decision_for_decision() {
        // Same config, same traffic: GCRA and the token bucket must agree exactly.
        let clock = ManualClock::new();
        let config = RateLimitConfig::new(7, Duration::from_nanos(1_000_003)).unwrap();
        let gcra = Gcra::with_clock(config, &clock);
        let bucket = TokenBucket::with_clock(config, &clock);
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..5_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            clock.advance(Duration::from_nanos(x % 400_000));
            let cost = Cost(x % 9);
            assert_eq!(gcra.check("k", cost), bucket.check("k", cost));
        }
    }

    #[test]
    fn is_listed_in_all() {
        assert!(Algorithm::ALL.contains(&Algorithm::Gcra));
        assert_eq!("gcra".parse::<Algorithm>(), Ok(Algorithm::Gcra));
    }
}
