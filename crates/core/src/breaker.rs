//! Circuit breaker for a remote rate limit backend, plus the policy for what to decide
//! while the backend is unavailable.
//!
//! The breaker has the usual three states:
//!
//! - **Closed:** calls go to the backend. After `failure_threshold` failures in a row it
//!   opens.
//! - **Open:** calls are not attempted for `open_for`; the caller applies its
//!   [`FailurePolicy`] instead.
//! - **Half-open:** after `open_for`, one probe call is let through. Success closes the
//!   breaker, failure opens it again for another `open_for`.
//!
//! Time comes from a [`Clock`], so every transition can be tested without sleeping.

use crate::{Clock, Decision, Nanos, Tokens};
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use thiserror::Error;

/// Why a [`BreakerConfig`] could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BreakerConfigError {
    /// `failure_threshold` was zero.
    #[error("failure threshold must be at least 1")]
    ZeroThreshold,
    /// `open_for` was zero.
    #[error("open duration must be longer than zero")]
    ZeroOpenFor,
}

/// When the breaker opens and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BreakerConfig {
    failure_threshold: u32,
    open_for: Duration,
}

impl BreakerConfig {
    /// Opens after `failure_threshold` consecutive failures and stays open for `open_for`.
    ///
    /// ```
    /// use ratelab::breaker::{BreakerConfig, BreakerConfigError};
    /// use std::time::Duration;
    ///
    /// let config = BreakerConfig::new(3, Duration::from_millis(500))?;
    /// assert_eq!(config.failure_threshold(), 3);
    /// assert!(BreakerConfig::new(0, Duration::from_secs(1)).is_err());
    /// # Ok::<(), BreakerConfigError>(())
    /// ```
    pub fn new(failure_threshold: u32, open_for: Duration) -> Result<Self, BreakerConfigError> {
        if failure_threshold == 0 {
            return Err(BreakerConfigError::ZeroThreshold);
        }
        if open_for.is_zero() {
            return Err(BreakerConfigError::ZeroOpenFor);
        }
        Ok(Self {
            failure_threshold,
            open_for,
        })
    }

    /// Consecutive failures that open the breaker.
    ///
    /// ```
    /// use ratelab::BreakerConfig;
    /// use std::time::Duration;
    ///
    /// assert_eq!(BreakerConfig::new(3, Duration::from_secs(1))?.failure_threshold(), 3);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn failure_threshold(&self) -> u32 {
        self.failure_threshold
    }

    /// How long the breaker stays open before it lets a probe through.
    ///
    /// ```
    /// use ratelab::BreakerConfig;
    /// use std::time::Duration;
    ///
    /// let c = BreakerConfig::new(3, Duration::from_millis(500))?;
    /// assert_eq!(c.open_for(), Duration::from_millis(500));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn open_for(&self) -> Duration {
        self.open_for
    }
}

/// The breaker's current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Calls go to the backend.
    Closed,
    /// Calls are not attempted.
    Open,
    /// One probe call is being let through.
    HalfOpen,
}

impl BreakerState {
    /// Lower-case name, as used in logs and metrics.
    ///
    /// ```
    /// use ratelab::breaker::BreakerState;
    /// assert_eq!(BreakerState::HalfOpen.name(), "half_open");
    /// ```
    pub fn name(self) -> &'static str {
        match self {
            BreakerState::Closed => "closed",
            BreakerState::Open => "open",
            BreakerState::HalfOpen => "half_open",
        }
    }
}

#[derive(Debug)]
enum Inner {
    Closed { failures: u32 },
    Open { until: Nanos },
    HalfOpen { probe_started: Nanos },
}

/// Returned by [`CircuitBreaker::acquire`] when the backend must not be called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejected {
    /// Time until the breaker will let a probe through (zero while a probe is running).
    pub retry_after: Duration,
}

/// A thread-safe circuit breaker.
///
/// ```
/// use ratelab::breaker::{BreakerConfig, BreakerState, CircuitBreaker};
/// use ratelab::ManualClock;
/// use std::time::Duration;
///
/// let clock = ManualClock::new();
/// let breaker = CircuitBreaker::new(BreakerConfig::new(2, Duration::from_secs(1))?, &clock);
///
/// breaker.record_failure();
/// breaker.record_failure();
/// assert_eq!(breaker.state(), BreakerState::Open);
/// assert!(breaker.acquire().is_err());
///
/// clock.advance(Duration::from_secs(1));
/// assert!(breaker.acquire().is_ok()); // the probe
/// breaker.record_success();
/// assert_eq!(breaker.state(), BreakerState::Closed);
/// # Ok::<(), ratelab::breaker::BreakerConfigError>(())
/// ```
#[derive(Debug)]
pub struct CircuitBreaker<C> {
    config: BreakerConfig,
    clock: C,
    inner: Mutex<Inner>,
}

impl<C: Clock> CircuitBreaker<C> {
    /// A closed breaker that reads time from `clock`.
    ///
    /// ```
    /// use ratelab::{BreakerConfig, BreakerState, CircuitBreaker, ManualClock};
    /// use std::time::Duration;
    ///
    /// let clock = ManualClock::new();
    /// let breaker = CircuitBreaker::new(BreakerConfig::new(2, Duration::from_secs(1))?, &clock);
    /// assert_eq!(breaker.state(), BreakerState::Closed);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(config: BreakerConfig, clock: C) -> Self {
        Self {
            config,
            clock,
            inner: Mutex::new(Inner::Closed { failures: 0 }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // The state is always valid between statements, so a poisoned lock is safe to use.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks whether the backend may be called now.
    ///
    /// `Ok` means call it, then report the outcome with [`record_success`] or
    /// [`record_failure`]. In the half-open state only one caller gets `Ok`; if that
    /// probe never reports back, another probe is allowed after `open_for`.
    ///
    /// [`record_success`]: Self::record_success
    /// [`record_failure`]: Self::record_failure
    ///
    /// ```
    /// use ratelab::{BreakerConfig, BreakerState, CircuitBreaker, ManualClock};
    /// use std::time::Duration;
    ///
    /// let clock = ManualClock::new();
    /// let breaker = CircuitBreaker::new(BreakerConfig::new(2, Duration::from_secs(1))?, &clock);
    /// breaker.record_failure();
    /// breaker.record_failure();
    /// assert!(breaker.acquire().is_err()); // open: do not call the backend
    /// clock.advance(Duration::from_secs(1));
    /// assert!(breaker.acquire().is_ok()); // one probe may go
    /// assert_eq!(breaker.state(), BreakerState::HalfOpen);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn acquire(&self) -> Result<(), Rejected> {
        let now = self.clock.now();
        let mut inner = self.lock();
        match *inner {
            Inner::Closed { .. } => Ok(()),
            Inner::Open { until } if now >= until => {
                *inner = Inner::HalfOpen { probe_started: now };
                Ok(())
            }
            Inner::Open { until } => Err(Rejected {
                retry_after: until.saturating_since(now),
            }),
            Inner::HalfOpen { probe_started }
                if now.saturating_since(probe_started) >= self.config.open_for =>
            {
                *inner = Inner::HalfOpen { probe_started: now };
                Ok(())
            }
            Inner::HalfOpen { .. } => Err(Rejected {
                retry_after: Duration::ZERO,
            }),
        }
    }

    /// Reports a successful backend call. Closes the breaker.
    ///
    /// ```
    /// use ratelab::{BreakerConfig, BreakerState, CircuitBreaker, ManualClock};
    /// use std::time::Duration;
    ///
    /// let clock = ManualClock::new();
    /// let breaker = CircuitBreaker::new(BreakerConfig::new(2, Duration::from_secs(1))?, &clock);
    /// breaker.record_failure();
    /// breaker.record_success(); // the count starts again
    /// breaker.record_failure();
    /// assert_eq!(breaker.state(), BreakerState::Closed);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn record_success(&self) {
        *self.lock() = Inner::Closed { failures: 0 };
    }

    /// Reports a failed backend call. Opens the breaker after `failure_threshold`
    /// consecutive failures, or straight away if the failed call was the half-open probe.
    ///
    /// ```
    /// use ratelab::{BreakerConfig, BreakerState, CircuitBreaker, ManualClock};
    /// use std::time::Duration;
    ///
    /// let clock = ManualClock::new();
    /// let breaker = CircuitBreaker::new(BreakerConfig::new(2, Duration::from_secs(1))?, &clock);
    /// breaker.record_failure();
    /// assert_eq!(breaker.state(), BreakerState::Closed);
    /// breaker.record_failure();
    /// assert_eq!(breaker.state(), BreakerState::Open);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn record_failure(&self) {
        let now = self.clock.now();
        let open = Inner::Open {
            until: now.saturating_add(self.config.open_for),
        };
        let mut inner = self.lock();
        match *inner {
            Inner::Closed { failures } => {
                let failures = failures.saturating_add(1);
                *inner = if failures >= self.config.failure_threshold {
                    open
                } else {
                    Inner::Closed { failures }
                };
            }
            Inner::HalfOpen { .. } => *inner = open,
            // A late failure from a call started before the breaker opened.
            Inner::Open { .. } => {}
        }
    }

    /// The current state. An open breaker whose `open_for` has passed still reports
    /// `Open` until the next [`acquire`](Self::acquire) turns it half-open.
    ///
    /// ```
    /// use ratelab::{BreakerConfig, BreakerState, CircuitBreaker, ManualClock};
    /// use std::time::Duration;
    ///
    /// let clock = ManualClock::new();
    /// let breaker = CircuitBreaker::new(BreakerConfig::new(2, Duration::from_secs(1))?, &clock);
    /// assert_eq!(breaker.state(), BreakerState::Closed);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn state(&self) -> BreakerState {
        match *self.lock() {
            Inner::Closed { .. } => BreakerState::Closed,
            Inner::Open { .. } => BreakerState::Open,
            Inner::HalfOpen { .. } => BreakerState::HalfOpen,
        }
    }

    /// The configuration this breaker was built with.
    ///
    /// ```
    /// use ratelab::{BreakerConfig, BreakerState, CircuitBreaker, ManualClock};
    /// use std::time::Duration;
    ///
    /// let clock = ManualClock::new();
    /// let breaker = CircuitBreaker::new(BreakerConfig::new(2, Duration::from_secs(1))?, &clock);
    /// assert_eq!(breaker.config().failure_threshold(), 2);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn config(&self) -> BreakerConfig {
        self.config
    }
}

/// What to decide when the backend cannot answer (breaker open, timeout, error).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailurePolicy {
    /// Allow the request. Keeps the service available; the limit is not enforced.
    #[default]
    FailOpen,
    /// Deny the request. Keeps the limit; the service is unavailable to limited callers.
    FailClosed,
}

impl FailurePolicy {
    /// Lower-case name: `open` or `closed`.
    ///
    /// ```
    /// use ratelab::breaker::FailurePolicy;
    /// assert_eq!(FailurePolicy::FailClosed.name(), "closed");
    /// assert_eq!("open".parse::<FailurePolicy>(), Ok(FailurePolicy::FailOpen));
    /// ```
    pub fn name(self) -> &'static str {
        match self {
            FailurePolicy::FailOpen => "open",
            FailurePolicy::FailClosed => "closed",
        }
    }

    /// The decision to return without the backend. `retry_after` is the hint given to
    /// a denied caller.
    ///
    /// ```
    /// use ratelab::breaker::FailurePolicy;
    /// use std::time::Duration;
    ///
    /// assert!(FailurePolicy::FailOpen.decide(Duration::from_secs(1)).allowed);
    /// let denied = FailurePolicy::FailClosed.decide(Duration::from_secs(1));
    /// assert!(!denied.allowed);
    /// assert_eq!(denied.retry_after, Some(Duration::from_secs(1)));
    /// ```
    pub fn decide(self, retry_after: Duration) -> Decision {
        match self {
            FailurePolicy::FailOpen => Decision::allow(Tokens(0), Duration::ZERO),
            FailurePolicy::FailClosed => Decision::deny(Tokens(0), retry_after, Some(retry_after)),
        }
    }
}

/// A failure policy name that is neither `open` nor `closed`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown failure policy {0:?} (expected \"open\" or \"closed\")")]
pub struct UnknownPolicy(pub String);

impl FromStr for FailurePolicy {
    type Err = UnknownPolicy;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "open" | "fail_open" | "fail-open" => Ok(FailurePolicy::FailOpen),
            "closed" | "fail_closed" | "fail-closed" => Ok(FailurePolicy::FailClosed),
            other => Err(UnknownPolicy(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManualClock;

    fn breaker(clock: &ManualClock) -> CircuitBreaker<&ManualClock> {
        CircuitBreaker::new(
            BreakerConfig::new(3, Duration::from_millis(100)).unwrap(),
            clock,
        )
    }

    #[test]
    fn opens_after_threshold_consecutive_failures() {
        let clock = ManualClock::new();
        let b = breaker(&clock);
        b.record_failure();
        b.record_failure();
        assert_eq!(b.state(), BreakerState::Closed);
        b.record_failure();
        assert_eq!(b.state(), BreakerState::Open);
    }

    #[test]
    fn success_resets_the_failure_count() {
        let clock = ManualClock::new();
        let b = breaker(&clock);
        b.record_failure();
        b.record_failure();
        b.record_success();
        b.record_failure();
        b.record_failure();
        assert_eq!(b.state(), BreakerState::Closed);
    }

    #[test]
    fn open_rejects_with_exact_remaining_time() {
        let clock = ManualClock::new();
        let b = breaker(&clock);
        for _ in 0..3 {
            b.record_failure();
        }
        clock.advance(Duration::from_millis(30));
        assert_eq!(
            b.acquire(),
            Err(Rejected {
                retry_after: Duration::from_millis(70)
            })
        );
        clock.advance(Duration::from_millis(70) - Duration::from_nanos(1));
        assert!(b.acquire().is_err());
        clock.advance(Duration::from_nanos(1));
        assert!(b.acquire().is_ok());
        assert_eq!(b.state(), BreakerState::HalfOpen);
    }

    #[test]
    fn half_open_lets_exactly_one_probe_through() {
        let clock = ManualClock::new();
        let b = breaker(&clock);
        for _ in 0..3 {
            b.record_failure();
        }
        clock.advance(Duration::from_millis(100));
        assert!(b.acquire().is_ok());
        assert_eq!(
            b.acquire(),
            Err(Rejected {
                retry_after: Duration::ZERO
            })
        );
    }

    #[test]
    fn probe_success_closes_probe_failure_reopens() {
        let clock = ManualClock::new();
        let b = breaker(&clock);
        for _ in 0..3 {
            b.record_failure();
        }
        clock.advance(Duration::from_millis(100));
        assert!(b.acquire().is_ok());
        b.record_failure();
        assert_eq!(b.state(), BreakerState::Open);
        // Reopened for a full open_for from the probe failure.
        clock.advance(Duration::from_millis(99));
        assert!(b.acquire().is_err());
        clock.advance(Duration::from_millis(1));
        assert!(b.acquire().is_ok());
        b.record_success();
        assert_eq!(b.state(), BreakerState::Closed);
        assert!(b.acquire().is_ok());
    }

    #[test]
    fn lost_probe_is_replaced_after_open_for() {
        let clock = ManualClock::new();
        let b = breaker(&clock);
        for _ in 0..3 {
            b.record_failure();
        }
        clock.advance(Duration::from_millis(100));
        assert!(b.acquire().is_ok()); // this probe never reports back
        clock.advance(Duration::from_millis(99));
        assert!(b.acquire().is_err());
        clock.advance(Duration::from_millis(1));
        assert!(b.acquire().is_ok());
    }

    #[test]
    fn late_failure_while_open_does_not_extend_it() {
        let clock = ManualClock::new();
        let b = breaker(&clock);
        for _ in 0..3 {
            b.record_failure();
        }
        clock.advance(Duration::from_millis(50));
        b.record_failure();
        clock.advance(Duration::from_millis(50));
        assert!(b.acquire().is_ok());
    }

    #[test]
    fn config_rejects_zero_values() {
        assert_eq!(
            BreakerConfig::new(0, Duration::from_secs(1)),
            Err(BreakerConfigError::ZeroThreshold)
        );
        assert_eq!(
            BreakerConfig::new(1, Duration::ZERO),
            Err(BreakerConfigError::ZeroOpenFor)
        );
    }

    #[test]
    fn policy_parsing() {
        assert_eq!("closed".parse(), Ok(FailurePolicy::FailClosed));
        assert_eq!("fail-open".parse(), Ok(FailurePolicy::FailOpen));
        assert!("maybe".parse::<FailurePolicy>().is_err());
    }

    #[test]
    fn breaker_is_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CircuitBreaker<std::sync::Arc<ManualClock>>>();
    }
}
