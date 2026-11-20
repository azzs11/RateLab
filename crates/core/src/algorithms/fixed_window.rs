//! Fixed window: time is cut into back-to-back windows of length `window` (aligned to
//! the clock origin); each key may spend `limit` units per window.

use super::with_state;
use crate::{Clock, Cost, Decision, RateLimitConfig, RateLimiter, Tokens};
use dashmap::DashMap;
use std::time::Duration;

#[derive(Debug)]
struct Window {
    index: u64,
    used: u64,
}

/// Fixed window counter.
///
/// The cheapest algorithm: one counter per key. Its known weakness is the window
/// boundary: a client can spend `limit` at the end of one window and `limit` again at
/// the start of the next, so a short span can admit up to `2 × limit`.
///
/// ```
/// use ratelab::{Cost, FixedWindow, ManualClock, RateLimitConfig, RateLimiter};
/// use std::time::Duration;
///
/// let clock = ManualClock::new();
/// let limiter = FixedWindow::with_clock(RateLimitConfig::per_second(3)?, &clock);
/// assert!(limiter.check("k", Cost(3)).allowed);
/// assert!(!limiter.check("k", Cost::ONE).allowed);
///
/// clock.advance(Duration::from_secs(1)); // next window
/// assert!(limiter.check("k", Cost(3)).allowed);
/// # Ok::<(), ratelab::ConfigError>(())
/// ```
#[derive(Debug)]
pub struct FixedWindow<C> {
    limit: u64,
    window: u64,
    clock: C,
    windows: DashMap<String, Window>,
}

impl<C: Clock> FixedWindow<C> {
    /// A fixed window limiter that reads time from `clock`.
    ///
    /// ```
    /// use ratelab::{Cost, ManualClock, RateLimitConfig, RateLimiter, FixedWindow};
    ///
    /// let clock = ManualClock::new();
    /// let limiter = FixedWindow::with_clock(RateLimitConfig::per_second(1)?, &clock);
    /// assert!(limiter.check("k", Cost::ONE).allowed);
    /// assert!(!limiter.check("k", Cost::ONE).allowed);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn with_clock(config: RateLimitConfig, clock: C) -> Self {
        Self {
            limit: config.limit(),
            window: config.window_nanos(),
            clock,
            windows: DashMap::new(),
        }
    }
}

#[cfg(feature = "std-clock")]
impl FixedWindow<crate::MonotonicClock> {
    /// A fixed window limiter on the real monotonic clock.
    ///
    /// ```
    /// use ratelab::{Cost, RateLimitConfig, RateLimiter, FixedWindow};
    ///
    /// let limiter = FixedWindow::new(RateLimitConfig::per_minute(100)?);
    /// assert!(limiter.check("k", Cost::ONE).allowed);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn new(config: RateLimitConfig) -> Self {
        Self::with_clock(config, crate::MonotonicClock::new())
    }
}

impl<C: Clock> RateLimiter for FixedWindow<C> {
    fn check(&self, key: &str, cost: Cost) -> Decision {
        let now = self.clock.now().0;
        let index = now / self.window;
        let window_end = (u128::from(index) + 1) * u128::from(self.window);
        let until_next =
            Duration::from_nanos(u64::try_from(window_end - u128::from(now)).unwrap_or(u64::MAX));

        with_state(
            &self.windows,
            key,
            || Window { index, used: 0 },
            |w| {
                if w.index != index {
                    w.index = index;
                    w.used = 0;
                }

                let allowed = w.used.saturating_add(cost.0) <= self.limit;
                if allowed {
                    w.used += cost.0;
                }
                let remaining = Tokens(self.limit - w.used);
                let reset_after = if w.used == 0 {
                    Duration::ZERO
                } else {
                    until_next
                };

                if allowed {
                    Decision::allow(remaining, reset_after)
                } else {
                    let retry_after = (cost.0 <= self.limit).then_some(until_next);
                    Decision::deny(remaining, reset_after, retry_after)
                }
            },
        )
    }

    fn reset(&self, key: &str) {
        self.windows.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ManualClock, Nanos};

    #[test]
    fn allows_limit_per_window() {
        let clock = ManualClock::new();
        let l = FixedWindow::with_clock(RateLimitConfig::per_second(5).unwrap(), &clock);
        for _ in 0..5 {
            assert!(l.check("user1", Cost::ONE).allowed);
        }
        assert!(!l.check("user1", Cost::ONE).allowed);
    }

    #[test]
    fn resets_at_the_next_window() {
        let clock = ManualClock::new();
        let config = RateLimitConfig::new(3, Duration::from_millis(500)).unwrap();
        let l = FixedWindow::with_clock(config, &clock);
        for _ in 0..3 {
            assert!(l.check("user1", Cost::ONE).allowed);
        }
        let d = l.check("user1", Cost::ONE);
        assert!(!d.allowed);
        assert_eq!(d.retry_after, Some(Duration::from_millis(500)));

        clock.advance(Duration::from_millis(500));
        assert!(l.check("user1", Cost::ONE).allowed);
    }

    #[test]
    fn boundary_burst_admits_twice_the_limit() {
        // Documents the known weakness rather than hiding it.
        let clock = ManualClock::starting_at(Nanos(999_000_000));
        let l = FixedWindow::with_clock(RateLimitConfig::per_second(10).unwrap(), &clock);
        assert!(l.check("k", Cost(10)).allowed);
        clock.advance(Duration::from_millis(2)); // crosses into the next window
        assert!(l.check("k", Cost(10)).allowed);
    }
}
