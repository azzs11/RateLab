//! The single-node algorithms.
//!
//! All of them keep per-key state in a [`DashMap`], take time from an injected
//! [`Clock`](crate::Clock), and use integer arithmetic only, so the same inputs always
//! give the same decisions on every platform.

pub mod fixed_window;
pub mod gcra;
pub mod leaky_bucket;
pub mod sliding_window;
pub mod token_bucket;

pub use fixed_window::FixedWindow;
pub use gcra::Gcra;
pub use leaky_bucket::LeakyBucket;
pub use sliding_window::SlidingWindow;
pub use token_bucket::TokenBucket;

use crate::{Clock, RateLimitConfig, RateLimiter};
use dashmap::DashMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

/// Names every algorithm, for choosing one at runtime (from a config file, an HTTP
/// request, or the browser playground).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Algorithm {
    /// [`TokenBucket`]
    TokenBucket,
    /// [`LeakyBucket`]
    LeakyBucket,
    /// [`FixedWindow`]
    FixedWindow,
    /// [`SlidingWindow`]
    SlidingWindow,
    /// [`Gcra`]
    Gcra,
}

impl Algorithm {
    /// Every algorithm, in a stable order.
    pub const ALL: [Algorithm; 5] = [
        Algorithm::TokenBucket,
        Algorithm::LeakyBucket,
        Algorithm::FixedWindow,
        Algorithm::SlidingWindow,
        Algorithm::Gcra,
    ];

    /// The snake_case name used by [`FromStr`] and [`Display`](fmt::Display).
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::TokenBucket => "token_bucket",
            Algorithm::LeakyBucket => "leaky_bucket",
            Algorithm::FixedWindow => "fixed_window",
            Algorithm::SlidingWindow => "sliding_window",
            Algorithm::Gcra => "gcra",
        }
    }

    /// Builds a boxed limiter of this kind.
    ///
    /// ```
    /// use drl_core::{Algorithm, Cost, ManualClock, RateLimitConfig};
    ///
    /// let algorithm: Algorithm = "fixed_window".parse()?;
    /// let limiter = algorithm.build(RateLimitConfig::per_second(1)?, ManualClock::new());
    /// assert!(limiter.check("k", Cost::ONE).allowed);
    /// assert!(!limiter.check("k", Cost::ONE).allowed);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn build<C: Clock + 'static>(
        self,
        config: RateLimitConfig,
        clock: C,
    ) -> Box<dyn RateLimiter> {
        match self {
            Algorithm::TokenBucket => Box::new(TokenBucket::with_clock(config, clock)),
            Algorithm::LeakyBucket => Box::new(LeakyBucket::with_clock(config, clock)),
            Algorithm::FixedWindow => Box::new(FixedWindow::with_clock(config, clock)),
            Algorithm::SlidingWindow => Box::new(SlidingWindow::with_clock(config, clock)),
            Algorithm::Gcra => Box::new(Gcra::with_clock(config, clock)),
        }
    }
}

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The name passed to [`Algorithm::from_str`] matched no algorithm.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown algorithm `{0}`")]
pub struct UnknownAlgorithm(pub String);

impl FromStr for Algorithm {
    type Err = UnknownAlgorithm;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Algorithm::ALL
            .into_iter()
            .find(|a| a.name() == s)
            .ok_or_else(|| UnknownAlgorithm(s.to_owned()))
    }
}

/// Runs `f` on the state for `key`, creating it with `init` first if needed.
///
/// The DashMap shard lock is held for the whole call, so check-and-update is atomic
/// per key. The common path (key already present) does not allocate.
pub(crate) fn with_state<S, R>(
    map: &DashMap<String, S>,
    key: &str,
    init: impl FnOnce() -> S,
    f: impl FnOnce(&mut S) -> R,
) -> R {
    if let Some(mut state) = map.get_mut(key) {
        return f(&mut state);
    }
    let mut state = map.entry(key.to_owned()).or_insert_with(init);
    f(&mut state)
}

/// `ceil(num / den)` nanoseconds as a `Duration`, saturating at `u64::MAX` ns.
/// `den` must be non-zero.
pub(crate) fn ceil_div_nanos(num: u128, den: u128) -> Duration {
    let q = num.div_ceil(den);
    Duration::from_nanos(u64::try_from(q).unwrap_or(u64::MAX))
}

/// A `u128` count saturated into `u64`.
pub(crate) fn sat_u64(x: u128) -> u64 {
    u64::try_from(x).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for a in Algorithm::ALL {
            assert_eq!(a.name().parse::<Algorithm>(), Ok(a));
            assert_eq!(a.to_string(), a.name());
        }
        assert_eq!(
            "nope".parse::<Algorithm>(),
            Err(UnknownAlgorithm("nope".into()))
        );
    }

    #[test]
    fn ceil_div_rounds_up_and_saturates() {
        assert_eq!(ceil_div_nanos(10, 5), Duration::from_nanos(2));
        assert_eq!(ceil_div_nanos(11, 5), Duration::from_nanos(3));
        assert_eq!(ceil_div_nanos(0, 5), Duration::ZERO);
        assert_eq!(ceil_div_nanos(u128::MAX, 1), Duration::from_nanos(u64::MAX));
    }
}
