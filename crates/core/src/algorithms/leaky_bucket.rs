//! Leaky bucket (as a meter): each request pours `cost` units into a bucket of size
//! `limit` that drains continuously at `limit` per `window`. A request that would
//! overflow the bucket is rejected.

use super::{ceil_div_nanos, sat_u64, with_state};
use crate::{Clock, Cost, Decision, RateLimitConfig, RateLimiter, Tokens};
use dashmap::DashMap;

/// Per-key state. Water is scaled by `window_nanos` so draining is exact in integers:
/// one nanosecond drains `limit` scaled units.
#[derive(Debug)]
struct Bucket {
    water: u128,
    last: u64,
}

/// Leaky bucket limiter (meter form).
///
/// Starts empty, so it admits a burst of up to `limit` units, then admits work only as
/// fast as the bucket drains.
///
/// ```
/// use ratelab::{Cost, LeakyBucket, ManualClock, RateLimitConfig, RateLimiter};
/// use std::time::Duration;
///
/// let clock = ManualClock::new();
/// let limiter = LeakyBucket::with_clock(RateLimitConfig::per_second(2)?, &clock);
/// assert!(limiter.check("k", Cost(2)).allowed);
/// assert!(!limiter.check("k", Cost::ONE).allowed);
///
/// clock.advance(Duration::from_millis(500)); // half a window drains one unit
/// assert!(limiter.check("k", Cost::ONE).allowed);
/// # Ok::<(), ratelab::ConfigError>(())
/// ```
#[derive(Debug)]
pub struct LeakyBucket<C> {
    limit: u128,
    window: u128,
    clock: C,
    buckets: DashMap<String, Bucket>,
}

impl<C: Clock> LeakyBucket<C> {
    /// A leaky bucket that reads time from `clock`.
    ///
    /// ```
    /// use ratelab::{Cost, ManualClock, RateLimitConfig, RateLimiter, LeakyBucket};
    ///
    /// let clock = ManualClock::new();
    /// let limiter = LeakyBucket::with_clock(RateLimitConfig::per_second(1)?, &clock);
    /// assert!(limiter.check("k", Cost::ONE).allowed);
    /// assert!(!limiter.check("k", Cost::ONE).allowed);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
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
impl LeakyBucket<crate::MonotonicClock> {
    /// A leaky bucket on the real monotonic clock.
    ///
    /// ```
    /// use ratelab::{Cost, RateLimitConfig, RateLimiter, LeakyBucket};
    ///
    /// let limiter = LeakyBucket::new(RateLimitConfig::per_minute(100)?);
    /// assert!(limiter.check("k", Cost::ONE).allowed);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn new(config: RateLimitConfig) -> Self {
        Self::with_clock(config, crate::MonotonicClock::new())
    }
}

impl<C: Clock> RateLimiter for LeakyBucket<C> {
    fn check(&self, key: &str, cost: Cost) -> Decision {
        let now = self.clock.now().0;
        let capacity = self.limit * self.window;
        let need = u128::from(cost.0) * self.window;

        with_state(
            &self.buckets,
            key,
            || Bucket {
                water: 0,
                last: now,
            },
            |b| {
                let elapsed = u128::from(now.saturating_sub(b.last));
                b.water = b.water.saturating_sub(elapsed.saturating_mul(self.limit));
                b.last = b.last.max(now);

                let allowed = b.water.saturating_add(need) <= capacity;
                if allowed {
                    b.water += need;
                }
                let remaining = Tokens(sat_u64((capacity - b.water) / self.window));
                let reset_after = ceil_div_nanos(b.water, self.limit);

                if allowed {
                    Decision::allow(remaining, reset_after)
                } else {
                    let retry_after = (u128::from(cost.0) <= self.limit)
                        .then(|| ceil_div_nanos(b.water + need - capacity, self.limit));
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

    fn limiter(limit: u64, clock: &ManualClock) -> LeakyBucket<&ManualClock> {
        LeakyBucket::with_clock(RateLimitConfig::per_second(limit).unwrap(), clock)
    }

    #[test]
    fn starts_empty_and_overflows_at_capacity() {
        let clock = ManualClock::new();
        let l = limiter(3, &clock);
        for _ in 0..3 {
            assert!(l.check("user1", Cost::ONE).allowed);
        }
        assert!(!l.check("user1", Cost::ONE).allowed);
    }

    #[test]
    fn leaks_over_time() {
        let clock = ManualClock::new();
        let l = limiter(2, &clock);
        assert!(l.check("user1", Cost::ONE).allowed);
        assert!(l.check("user1", Cost::ONE).allowed);
        assert!(!l.check("user1", Cost::ONE).allowed);

        clock.advance(Duration::from_secs(1));
        assert!(l.check("user1", Cost::ONE).allowed);
        assert!(l.check("user1", Cost::ONE).allowed);
    }

    #[test]
    fn retry_after_is_time_to_drain_the_overflow() {
        let clock = ManualClock::new();
        let l = limiter(4, &clock);
        assert!(l.check("k", Cost(4)).allowed);
        let d = l.check("k", Cost(2));
        assert!(!d.allowed);
        // 4 per second drains one unit every 250 ms; 2 units need 500 ms.
        assert_eq!(d.retry_after, Some(Duration::from_millis(500)));
        assert_eq!(d.reset_after, Duration::from_secs(1));
    }
}
