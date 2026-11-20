//! The shared store every strategy coordinates through: what Redis runs.
//!
//! One exact token bucket per key holds the **global** limit. Strategies talk to it
//! only with [`StoreOp`] messages, so the same logic can be replayed in the simulator
//! or implemented as Lua scripts in Redis.

use super::bucket::ShareBucket;
use super::{StoreOp, StoreReply};
use crate::{Nanos, RateLimitConfig};
use std::collections::HashMap;

/// The global limit, one bucket per key.
///
/// ```
/// use ratelab::strategies::{Store, StoreOp, StoreReply};
/// use ratelab::{Nanos, RateLimitConfig};
///
/// let mut store = Store::new(RateLimitConfig::per_second(10)?);
/// let reply = store.handle(Nanos::ZERO, "k", &StoreOp::Lease { want: 4 });
/// assert_eq!(reply, StoreReply::Leased { granted: 4, retry_after: None });
/// assert_eq!(store.tokens(Nanos::ZERO, "k"), 6);
/// # Ok::<(), ratelab::ConfigError>(())
/// ```
#[derive(Debug, Clone)]
pub struct Store {
    config: RateLimitConfig,
    buckets: HashMap<String, ShareBucket>,
    ops: u64,
}

impl Store {
    /// An empty store enforcing `config` per key.
    ///
    /// ```
    /// use ratelab::strategies::{Store, StoreOp};
    /// use ratelab::{Nanos, RateLimitConfig};
    ///
    /// let mut store = Store::new(RateLimitConfig::per_second(10)?);
    /// assert_eq!(store.tokens(Nanos::ZERO, "k"), 10);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: HashMap::new(),
            ops: 0,
        }
    }

    /// The limit it enforces.
    ///
    /// ```
    /// use ratelab::strategies::{Store, StoreOp};
    /// use ratelab::{Nanos, RateLimitConfig};
    ///
    /// let mut store = Store::new(RateLimitConfig::per_second(10)?);
    /// assert_eq!(store.config().limit(), 10);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn config(&self) -> RateLimitConfig {
        self.config
    }

    /// Operations handled so far.
    ///
    /// ```
    /// use ratelab::strategies::{Store, StoreOp};
    /// use ratelab::{Nanos, RateLimitConfig};
    ///
    /// let mut store = Store::new(RateLimitConfig::per_second(10)?);
    /// store.handle(Nanos::ZERO, "k", &StoreOp::Lease { want: 3 });
    /// assert_eq!(store.ops(), 1);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn ops(&self) -> u64 {
        self.ops
    }

    fn bucket(&mut self, key: &str, now: Nanos) -> &mut ShareBucket {
        let config = self.config;
        let bucket = self
            .buckets
            .entry(key.to_owned())
            .or_insert_with(|| ShareBucket::new(config, 1, now));
        bucket.refill(now);
        bucket
    }

    /// Applies one operation at store time `now`.
    ///
    /// ```
    /// use ratelab::strategies::{Store, StoreOp, StoreReply};
    /// use ratelab::{Nanos, RateLimitConfig};
    ///
    /// let mut store = Store::new(RateLimitConfig::per_second(10)?);
    /// let reply = store.handle(Nanos::ZERO, "k", &StoreOp::Sync { used: 3 });
    /// assert_eq!(reply, StoreReply::Synced { remaining: 7 });
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn handle(&mut self, now: Nanos, key: &str, op: &StoreOp) -> StoreReply {
        self.ops += 1;
        match *op {
            StoreOp::Check { cost, at } => {
                // `at` is the node's clock when the node supplies the time. That is
                // handled like an application-clock script (such as this project's
                // first Redis prototype): the caller's time is stored as is, even when
                // it is earlier than the last one. Otherwise the store uses its own clock.
                let bucket = match at {
                    Some(t) => {
                        let bucket = self.bucket(key, Nanos::ZERO);
                        bucket.refill_trusting(t);
                        bucket
                    }
                    None => self.bucket(key, now),
                };
                let allowed = bucket.try_take(cost.0);
                StoreReply::Checked(bucket.decision(allowed, cost.0))
            }
            StoreOp::Lease { want } => {
                let config = self.config;
                let bucket = self.bucket(key, now);
                let granted = bucket.take_up_to(want);
                // Nothing to give: say when the whole lease will be there, so the node
                // does not come back for one token at a time.
                let retry_after = if granted == 0 {
                    bucket
                        .time_until(want)
                        .or_else(|| bucket.time_until(bucket_capacity_tokens(config)))
                } else {
                    None
                };
                StoreReply::Leased {
                    granted,
                    retry_after,
                }
            }
            StoreOp::Return { amount } => {
                self.bucket(key, now).put(amount);
                StoreReply::Returned
            }
            StoreOp::Sync { used } => {
                let bucket = self.bucket(key, now);
                bucket.take_saturating(used);
                StoreReply::Synced {
                    remaining: bucket.tokens(),
                }
            }
        }
    }

    /// Tokens available for `key` at `now` (does not count as an operation).
    ///
    /// ```
    /// use ratelab::strategies::{Store, StoreOp};
    /// use ratelab::{Nanos, RateLimitConfig};
    ///
    /// let mut store = Store::new(RateLimitConfig::per_second(10)?);
    /// store.handle(Nanos::ZERO, "k", &StoreOp::Sync { used: 3 });
    /// assert_eq!(store.tokens(Nanos::ZERO, "k"), 7);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn tokens(&mut self, now: Nanos, key: &str) -> u64 {
        self.bucket(key, now).tokens()
    }
}

/// The most tokens one bucket can hold.
fn bucket_capacity_tokens(config: RateLimitConfig) -> u64 {
    config.limit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cost;
    use std::time::Duration;

    fn store(limit: u64) -> Store {
        Store::new(RateLimitConfig::per_second(limit).unwrap())
    }

    #[test]
    fn check_is_an_exact_token_bucket() {
        let mut s = store(2);
        let check = |s: &mut Store, t: u64| match s.handle(
            Nanos(t),
            "k",
            &StoreOp::Check {
                cost: Cost::ONE,
                at: None,
            },
        ) {
            StoreReply::Checked(d) => d,
            other => panic!("{other:?}"),
        };
        assert!(check(&mut s, 0).allowed);
        assert!(check(&mut s, 0).allowed);
        let denied = check(&mut s, 0);
        assert_eq!(denied.retry_after, Some(Duration::from_millis(500)));
        assert!(check(&mut s, 500_000_000).allowed);
        assert_eq!(s.ops(), 4);
    }

    #[test]
    fn node_supplied_time_is_used_when_given() {
        let mut s = store(1);
        let at = |t| StoreOp::Check {
            cost: Cost::ONE,
            at: Some(Nanos(t)),
        };
        assert!(matches!(s.handle(Nanos(0), "k", &at(0)), StoreReply::Checked(d) if d.allowed));
        // Store time says no time passed, but the node claims a second did.
        assert!(
            matches!(s.handle(Nanos(0), "k", &at(1_000_000_000)), StoreReply::Checked(d) if d.allowed)
        );
        // A caller whose clock is behind resets the last update time, so the next
        // caller that is ahead refills again.
        assert!(matches!(s.handle(Nanos(0), "k", &at(0)), StoreReply::Checked(d) if !d.allowed));
        assert!(
            matches!(s.handle(Nanos(0), "k", &at(1_000_000_000)), StoreReply::Checked(d) if d.allowed)
        );
    }

    #[test]
    fn leases_are_partial_when_scarce_and_hint_a_retry() {
        let mut s = store(10);
        let lease = |s: &mut Store, want| s.handle(Nanos(0), "k", &StoreOp::Lease { want });
        assert_eq!(
            lease(&mut s, 8),
            StoreReply::Leased {
                granted: 8,
                retry_after: None
            }
        );
        assert_eq!(
            lease(&mut s, 8),
            StoreReply::Leased {
                granted: 2,
                retry_after: None
            }
        );
        assert_eq!(
            lease(&mut s, 8),
            StoreReply::Leased {
                granted: 0,
                retry_after: Some(Duration::from_millis(800))
            }
        );
    }

    #[test]
    fn returns_and_syncs_adjust_the_global_bucket() {
        let mut s = store(10);
        s.handle(Nanos(0), "k", &StoreOp::Lease { want: 10 });
        s.handle(Nanos(0), "k", &StoreOp::Return { amount: 4 });
        assert_eq!(s.tokens(Nanos(0), "k"), 4);
        assert_eq!(
            s.handle(Nanos(0), "k", &StoreOp::Sync { used: 3 }),
            StoreReply::Synced { remaining: 1 }
        );
        // More was spent than was left: the store stops at empty.
        assert_eq!(
            s.handle(Nanos(0), "k", &StoreOp::Sync { used: 5 }),
            StoreReply::Synced { remaining: 0 }
        );
    }
}
