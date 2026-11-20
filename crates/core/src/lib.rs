//! Rate limiting algorithms with injected time and thread-safe per-key state.
//!
//! Every algorithm implements [`RateLimiter`]: `check(&self, key, cost)` returns a
//! [`Decision`]. Limiters take `&self`, so one instance can be shared across threads
//! (for example in an `Arc`). Time comes from a [`Clock`], so tests and simulations can
//! use [`ManualClock`] instead of sleeping.
//!
//! ```
//! use drl_core::{Cost, ManualClock, RateLimitConfig, RateLimiter, TokenBucket};
//! use std::time::Duration;
//!
//! let clock = ManualClock::new();
//! let limiter = TokenBucket::with_clock(RateLimitConfig::per_second(2)?, &clock);
//!
//! assert!(limiter.check("alice", Cost::ONE).allowed);
//! assert!(limiter.check("alice", Cost::ONE).allowed);
//! let denied = limiter.check("alice", Cost::ONE);
//! assert!(!denied.allowed);
//!
//! // Wait exactly as long as the limiter says, then retry.
//! clock.advance(denied.retry_after.unwrap());
//! assert!(limiter.check("alice", Cost::ONE).allowed);
//! # Ok::<(), drl_core::ConfigError>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod algorithms;
pub mod breaker;
pub mod clock;
pub mod decision;
pub mod strategies;
pub mod units;

pub use algorithms::{Algorithm, FixedWindow, Gcra, LeakyBucket, SlidingWindow, TokenBucket};
pub use breaker::{BreakerConfig, BreakerState, CircuitBreaker, FailurePolicy};
#[cfg(feature = "std-clock")]
pub use clock::MonotonicClock;
pub use clock::{Clock, ManualClock};
pub use decision::Decision;
pub use units::{Cost, Nanos, Tokens};

use std::time::Duration;
use thiserror::Error;

/// Why a [`RateLimitConfig`] could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// The limit was zero.
    #[error("limit must be at least 1")]
    ZeroLimit,
    /// The window was zero.
    #[error("window must be longer than zero")]
    ZeroWindow,
    /// The window does not fit in 64-bit nanoseconds.
    #[error("window must fit in u64 nanoseconds")]
    WindowTooLong,
}

/// "At most `limit` units per `window`", validated on construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitConfig {
    limit: u64,
    window_nanos: u64,
}

impl RateLimitConfig {
    /// A limit of `limit` units per `window`.
    ///
    /// ```
    /// use drl_core::{ConfigError, RateLimitConfig};
    /// use std::time::Duration;
    ///
    /// let config = RateLimitConfig::new(100, Duration::from_secs(60))?;
    /// assert_eq!(config.limit(), 100);
    /// assert_eq!(RateLimitConfig::new(0, Duration::from_secs(1)), Err(ConfigError::ZeroLimit));
    /// # Ok::<(), ConfigError>(())
    /// ```
    pub fn new(limit: u64, window: Duration) -> Result<Self, ConfigError> {
        if limit == 0 {
            return Err(ConfigError::ZeroLimit);
        }
        if window.is_zero() {
            return Err(ConfigError::ZeroWindow);
        }
        let window_nanos =
            u64::try_from(window.as_nanos()).map_err(|_| ConfigError::WindowTooLong)?;
        Ok(Self {
            limit,
            window_nanos,
        })
    }

    /// `limit` units per second.
    ///
    /// ```
    /// use drl_core::RateLimitConfig;
    /// use std::time::Duration;
    ///
    /// let config = RateLimitConfig::per_second(10)?;
    /// assert_eq!(config.window(), Duration::from_secs(1));
    /// # Ok::<(), drl_core::ConfigError>(())
    /// ```
    pub fn per_second(limit: u64) -> Result<Self, ConfigError> {
        Self::new(limit, Duration::from_secs(1))
    }

    /// `limit` units per minute.
    ///
    /// ```
    /// use drl_core::RateLimitConfig;
    /// use std::time::Duration;
    ///
    /// let config = RateLimitConfig::per_minute(10)?;
    /// assert_eq!(config.window(), Duration::from_secs(60));
    /// # Ok::<(), drl_core::ConfigError>(())
    /// ```
    pub fn per_minute(limit: u64) -> Result<Self, ConfigError> {
        Self::new(limit, Duration::from_secs(60))
    }

    /// Units allowed per window. Always at least 1.
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// The window length. Always longer than zero.
    pub fn window(&self) -> Duration {
        Duration::from_nanos(self.window_nanos)
    }

    /// The window length in nanoseconds. Always at least 1.
    pub fn window_nanos(&self) -> u64 {
        self.window_nanos
    }
}

/// The contract every algorithm implements.
pub trait RateLimiter: Send + Sync {
    /// Decides whether a request from `key` costing `cost` units may proceed, and
    /// charges the cost if it may.
    fn check(&self, key: &str, cost: Cost) -> Decision;

    /// Forgets all state for `key`, giving it a full quota again.
    fn reset(&self, key: &str);
}

impl<L: RateLimiter + ?Sized> RateLimiter for Box<L> {
    fn check(&self, key: &str, cost: Cost) -> Decision {
        (**self).check(key, cost)
    }

    fn reset(&self, key: &str) {
        (**self).reset(key)
    }
}

impl<L: RateLimiter + ?Sized> RateLimiter for std::sync::Arc<L> {
    fn check(&self, key: &str, cost: Cost) -> Decision {
        (**self).check(key, cost)
    }

    fn reset(&self, key: &str) {
        (**self).reset(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_rejects_zero_limit_and_window() {
        assert_eq!(
            RateLimitConfig::new(0, Duration::from_secs(1)),
            Err(ConfigError::ZeroLimit)
        );
        assert_eq!(
            RateLimitConfig::new(1, Duration::ZERO),
            Err(ConfigError::ZeroWindow)
        );
        assert_eq!(
            RateLimitConfig::new(1, Duration::MAX),
            Err(ConfigError::WindowTooLong)
        );
    }

    #[test]
    fn config_keeps_values() {
        let c = RateLimitConfig::new(7, Duration::from_millis(250)).unwrap();
        assert_eq!(c.limit(), 7);
        assert_eq!(c.window(), Duration::from_millis(250));
        assert_eq!(c.window_nanos(), 250_000_000);
    }
}
