//! Token bucket: a bucket of `limit` tokens refills continuously at `limit` per
//! `window`; each request spends `cost` tokens.

use super::{ceil_div_nanos, sat_u64, with_state};
use crate::{Clock, Cost, Decision, RateLimitConfig, RateLimiter, Tokens};
use dashmap::DashMap;

/// Per-key state. Token amounts are scaled by `window_nanos` so refill is exact in
/// integers: one nanosecond adds `limit` scaled units.
#[derive(Debug)]
struct Bucket {
    level: u128,
    last: u64,
}

/// Token bucket limiter.
///
/// Allows a burst of up to `limit` units, then refills smoothly. In any span of length
/// `window` it can admit up to about `2 × limit` units (a full bucket plus one window
/// of refill).
///
/// ```
/// use ratelab::{Cost, ManualClock, RateLimitConfig, RateLimiter, TokenBucket};
/// use std::time::Duration;
///
/// let clock = ManualClock::new();
/// let limiter = TokenBucket::with_clock(RateLimitConfig::per_second(4)?, &clock);
/// for _ in 0..4 {
///     assert!(limiter.check("k", Cost::ONE).allowed);
/// }
/// assert!(!limiter.check("k", Cost::ONE).allowed);
///
/// clock.advance(Duration::from_millis(250)); // a quarter window refills one token
/// assert!(limiter.check("k", Cost::ONE).allowed);
/// # Ok::<(), ratelab::ConfigError>(())
/// ```
#[derive(Debug)]
pub struct TokenBucket<C> {
    limit: u128,
    window: u128,
    clock: C,
    buckets: DashMap<String, Bucket>,
}

impl<C: Clock> TokenBucket<C> {
    /// A token bucket that reads time from `clock`.
    pub fn with_clock(config: RateLimitConfig, clock: C) -> Self {
        Self {
            limit: u128::from(config.limit()),
            window: u128::from(config.window_nanos()),
            clock,
            buckets: DashMap::new(),
        }
    }
}

#[cfg(feature = "std-clock")]
impl TokenBucket<crate::MonotonicClock> {
    /// A token bucket on the real monotonic clock.
    pub fn new(config: RateLimitConfig) -> Self {
        Self::with_clock(config, crate::MonotonicClock::new())
    }
}

impl<C: Clock> RateLimiter for TokenBucket<C> {
    fn check(&self, key: &str, cost: Cost) -> Decision {
        let now = self.clock.now().0;
        let capacity = self.limit * self.window;
        let need = u128::from(cost.0) * self.window;

        with_state(
            &self.buckets,
            key,
            || Bucket {
                level: capacity,
                last: now,
            },
            |b| {
                let elapsed = u128::from(now.saturating_sub(b.last));
                b.level = b
                    .level
                    .saturating_add(elapsed.saturating_mul(self.limit))
                    .min(capacity);
                b.last = b.last.max(now);

                let allowed = b.level >= need;
                if allowed {
                    b.level -= need;
                }
                let remaining = Tokens(sat_u64(b.level / self.window));
                let reset_after = ceil_div_nanos(capacity - b.level, self.limit);

                if allowed {
                    Decision::allow(remaining, reset_after)
                } else {
                    let retry_after = (u128::from(cost.0) <= self.limit)
                        .then(|| ceil_div_nanos(need - b.level, self.limit));
                    Decision::deny(remaining, reset_after, retry_after)
                }
            },
        )
    }

    fn reset(&self, key: &str) {
        self.buckets.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManualClock;
    use std::time::Duration;

    fn limiter(limit: u64, clock: &ManualClock) -> TokenBucket<&ManualClock> {
        TokenBucket::with_clock(RateLimitConfig::per_second(limit).unwrap(), clock)
    }

    #[test]
    fn allows_a_full_burst_then_denies() {
        let clock = ManualClock::new();
        let l = limiter(5, &clock);
        for i in 0..5 {
            let d = l.check("user1", Cost::ONE);
            assert!(d.allowed);
            assert_eq!(d.remaining, Tokens(4 - i));
        }
        assert!(!l.check("user1", Cost::ONE).allowed);
    }

    #[test]
    fn refills_over_time() {
        let clock = ManualClock::new();
        let l = limiter(2, &clock);
        assert!(l.check("user1", Cost::ONE).allowed);
        assert!(l.check("user1", Cost::ONE).allowed);
        assert!(!l.check("user1", Cost::ONE).allowed);

        clock.advance(Duration::from_secs(1));
        assert!(l.check("user1", Cost::ONE).allowed);
        assert!(l.check("user1", Cost::ONE).allowed);
        assert!(!l.check("user1", Cost::ONE).allowed);
    }

    #[test]
    fn partial_refill_is_exact() {
        let clock = ManualClock::new();
        let l = limiter(10, &clock);
        assert!(l.check("k", Cost(10)).allowed);
        // 10 per second = one token every 100 ms.
        clock.advance(Duration::from_millis(99));
        assert!(!l.check("k", Cost::ONE).allowed);
        clock.advance(Duration::from_millis(1));
        assert!(l.check("k", Cost::ONE).allowed);
    }

    #[test]
    fn retry_after_reports_the_deficit() {
        let clock = ManualClock::new();
        let l = limiter(10, &clock);
        assert!(l.check("k", Cost(10)).allowed);
        let d = l.check("k", Cost(3));
        assert!(!d.allowed);
        assert_eq!(d.retry_after, Some(Duration::from_millis(300)));
        assert_eq!(d.reset_after, Duration::from_secs(1));
    }

    #[test]
    fn different_keys_are_independent() {
        let clock = ManualClock::new();
        let l = limiter(1, &clock);
        assert!(l.check("user1", Cost::ONE).allowed);
        assert!(l.check("user2", Cost::ONE).allowed);
        assert!(!l.check("user1", Cost::ONE).allowed);
    }
}
