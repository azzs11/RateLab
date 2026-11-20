//! An exact token bucket that can also lease, take back and hand out a share.
//!
//! Amounts are kept in integer "units". A bucket for `limit` tokens per `window`
//! nanoseconds holds up to `limit × window` units and refills `limit` units per
//! nanosecond. One token is `window × share` units: with `share = 1` that is the
//! whole limit; with `share = N` the same capacity and refill rate hold only `limit / N`
//! tokens, which is exactly one node's share without any rounding.

use crate::{Decision, Nanos, RateLimitConfig, Tokens};
use std::time::Duration;

/// Token bucket state in exact integer units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareBucket {
    unit: u128,
    capacity: u128,
    rate: u128,
    level: u128,
    last: u64,
}

impl ShareBucket {
    /// A full bucket of `config.limit() / share` tokens refilling at the same fraction
    /// of the limit, starting at `now`. `share` of 0 is treated as 1.
    ///
    /// ```
    /// use ratelab::strategies::ShareBucket;
    /// use ratelab::{Nanos, RateLimitConfig};
    ///
    /// let config = RateLimitConfig::per_second(10)?;
    /// let mut third = ShareBucket::new(config, 3, Nanos::ZERO);
    /// assert_eq!(third.tokens(), 3); // 10 / 3 = 3.33 tokens
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn new(config: RateLimitConfig, share: u64, now: Nanos) -> Self {
        Self::with_shares(config, share, share, now)
    }

    /// A full bucket holding `limit / capacity_share` tokens and refilling at
    /// `limit / rate_share` per window. Shares of 0 are treated as 1.
    ///
    /// ```
    /// use ratelab::strategies::ShareBucket;
    /// use ratelab::{Nanos, RateLimitConfig};
    ///
    /// // Can hold the whole limit, but refills at a third of the rate.
    /// let config = RateLimitConfig::per_second(9)?;
    /// let mut b = ShareBucket::with_shares(config, 1, 3, Nanos::ZERO);
    /// assert_eq!(b.take_up_to(100), 9);
    /// b.refill(Nanos(1_000_000_000));
    /// assert_eq!(b.tokens(), 3);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn with_shares(
        config: RateLimitConfig,
        capacity_share: u64,
        rate_share: u64,
        now: Nanos,
    ) -> Self {
        let window = u128::from(config.window_nanos());
        let limit = u128::from(config.limit());
        let rate_share = u128::from(rate_share.max(1));
        let unit = window * rate_share;
        let capacity = limit * unit / u128::from(capacity_share.max(1));
        Self {
            unit,
            capacity,
            rate: limit,
            level: capacity,
            last: now.0,
        }
    }

    /// Adds the refill since the last call. Time that moves backwards adds nothing.
    pub fn refill(&mut self, now: Nanos) {
        if now.0 > self.last {
            let added = u128::from(now.0 - self.last).saturating_mul(self.rate);
            self.level = self.level.saturating_add(added).min(self.capacity);
            self.last = now.0;
        }
    }

    /// Refill the way an application-clock script does it: always store the caller's
    /// time as the last update, even when it is earlier than the previous one. With
    /// skewed callers, every jump forward refills again.
    ///
    /// ```
    /// use ratelab::strategies::ShareBucket;
    /// use ratelab::{Nanos, RateLimitConfig};
    ///
    /// let config = RateLimitConfig::per_second(10)?;
    /// let mut b = ShareBucket::new(config, 1, Nanos(1_000_000_000));
    /// b.take_up_to(10);
    /// // No real time passes, but callers alternate between clocks 100 ms apart:
    /// // each jump forward refills one token (100 ms at 10 per second).
    /// for _ in 0..3 {
    ///     b.refill_trusting(Nanos(1_100_000_000));
    ///     b.refill_trusting(Nanos(1_000_000_000));
    /// }
    /// assert_eq!(b.tokens(), 3);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn refill_trusting(&mut self, now: Nanos) {
        self.refill(now);
        self.last = now.0;
    }

    /// Whole tokens that would be available at `now`, without changing the bucket.
    pub fn tokens_at(&self, now: Nanos) -> u64 {
        let added = u128::from(now.0.saturating_sub(self.last)).saturating_mul(self.rate);
        let level = self.level.saturating_add(added).min(self.capacity);
        u64::try_from(level / self.unit).unwrap_or(u64::MAX)
    }

    /// Whole tokens available.
    pub fn tokens(&self) -> u64 {
        u64::try_from(self.level / self.unit).unwrap_or(u64::MAX)
    }

    fn units(&self, tokens: u64) -> u128 {
        u128::from(tokens).saturating_mul(self.unit)
    }

    /// Takes `n` tokens if all are available.
    pub fn try_take(&mut self, n: u64) -> bool {
        let need = self.units(n);
        if need <= self.level {
            self.level -= need;
            true
        } else {
            false
        }
    }

    /// Takes as many of `n` tokens as are available and returns how many.
    pub fn take_up_to(&mut self, n: u64) -> u64 {
        let granted = n.min(self.tokens());
        self.level -= self.units(granted);
        granted
    }

    /// Removes `n` tokens, stopping at empty (tokens already spent elsewhere).
    pub fn take_saturating(&mut self, n: u64) {
        self.level = self.level.saturating_sub(self.units(n));
    }

    /// Puts `n` unused tokens back, up to capacity.
    pub fn put(&mut self, n: u64) {
        self.level = self.level.saturating_add(self.units(n)).min(self.capacity);
    }

    /// Sets the bucket to `n` tokens (capped at capacity).
    pub fn set_tokens(&mut self, n: u64) {
        self.level = self.units(n).min(self.capacity);
    }

    /// Time until `n` tokens are available; `None` if `n` exceeds the capacity.
    pub fn time_until(&self, n: u64) -> Option<Duration> {
        let need = self.units(n);
        if need > self.capacity {
            return None;
        }
        let ns = need.saturating_sub(self.level).div_ceil(self.rate);
        Some(Duration::from_nanos(u64::try_from(ns).unwrap_or(u64::MAX)))
    }

    /// The decision to report after an attempt to take `cost`.
    pub fn decision(&self, allowed: bool, cost: u64) -> Decision {
        let reset_ns = (self.capacity - self.level).div_ceil(self.rate);
        let reset = Duration::from_nanos(u64::try_from(reset_ns).unwrap_or(u64::MAX));
        if allowed {
            Decision::allow(Tokens(self.tokens()), reset)
        } else {
            Decision::deny(Tokens(self.tokens()), reset, self.time_until(cost))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(limit: u64, window_ms: u64) -> RateLimitConfig {
        RateLimitConfig::new(limit, Duration::from_millis(window_ms)).unwrap()
    }

    #[test]
    fn whole_bucket_refills_at_the_limit_rate() {
        let mut b = ShareBucket::new(config(4, 1000), 1, Nanos::ZERO);
        assert!(b.try_take(4));
        assert!(!b.try_take(1));
        assert_eq!(b.time_until(1), Some(Duration::from_millis(250)));
        b.refill(Nanos(249_999_999));
        assert!(!b.try_take(1));
        b.refill(Nanos(250_000_000));
        assert!(b.try_take(1));
    }

    #[test]
    fn a_share_holds_a_fraction_exactly() {
        // 10 per second split 3 ways: 3.33 tokens, refilling 3.33 per second.
        let mut b = ShareBucket::new(config(10, 1000), 3, Nanos::ZERO);
        assert_eq!(b.take_up_to(10), 3);
        // A third of a token is left; the other two thirds refill at 10/3 tokens
        // per second: (2/3) / (10/3) s = 200 ms.
        assert_eq!(b.time_until(1), Some(Duration::from_millis(200)));
    }

    #[test]
    fn take_up_to_put_and_saturating_take() {
        let mut b = ShareBucket::new(config(10, 1000), 1, Nanos::ZERO);
        assert_eq!(b.take_up_to(7), 7);
        assert_eq!(b.take_up_to(7), 3);
        b.put(5);
        assert_eq!(b.tokens(), 5);
        b.put(50);
        assert_eq!(b.tokens(), 10);
        b.take_saturating(25);
        assert_eq!(b.tokens(), 0);
        b.set_tokens(4);
        assert_eq!(b.tokens(), 4);
    }

    #[test]
    fn time_never_moves_backwards() {
        let mut b = ShareBucket::new(config(1, 1000), 1, Nanos(5_000));
        assert!(b.try_take(1));
        b.refill(Nanos(1_000));
        assert_eq!(b.tokens(), 0);
        assert_eq!(b.time_until(2), None);
    }

    #[test]
    fn decision_reports_remaining_reset_and_retry() {
        let mut b = ShareBucket::new(config(2, 1000), 1, Nanos::ZERO);
        b.try_take(2);
        let d = b.decision(false, 1);
        assert_eq!(d.remaining, Tokens(0));
        assert_eq!(d.reset_after, Duration::from_secs(1));
        assert_eq!(d.retry_after, Some(Duration::from_millis(500)));
    }
}
