//! Injected time.
//!
//! Algorithms never read the system time themselves; they ask a [`Clock`]. Tests and
//! simulations use [`ManualClock`] and move time explicitly, so they need no sleeps and
//! are fully deterministic.

use crate::units::Nanos;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// A monotonic source of time.
pub trait Clock: Send + Sync {
    /// The current time. Must never go backwards for a given clock.
    fn now(&self) -> Nanos;
}

impl<C: Clock + ?Sized> Clock for &C {
    fn now(&self) -> Nanos {
        (**self).now()
    }
}

impl<C: Clock + ?Sized> Clock for Arc<C> {
    fn now(&self) -> Nanos {
        (**self).now()
    }
}

/// A clock that only moves when told to. Use it for tests and simulations.
///
/// Share it with a limiter through an `Arc` (or a reference) and keep a handle to
/// advance it:
///
/// ```
/// use ratelab::{Clock, ManualClock, Nanos};
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// let clock = Arc::new(ManualClock::new());
/// let handle = Arc::clone(&clock);
/// handle.advance(Duration::from_millis(5));
/// assert_eq!(clock.now(), Nanos(5_000_000));
/// ```
#[derive(Debug, Default)]
pub struct ManualClock {
    nanos: AtomicU64,
}

impl ManualClock {
    /// A clock starting at [`Nanos::ZERO`].
    ///
    /// ```
    /// use ratelab::{Clock, ManualClock, Nanos};
    ///
    /// let clock = ManualClock::new();
    /// assert_eq!(clock.now(), Nanos::ZERO);
    /// ```
    pub fn new() -> Self {
        Self::starting_at(Nanos::ZERO)
    }

    /// A clock starting at `start`.
    ///
    /// ```
    /// use ratelab::{Clock, ManualClock, Nanos};
    ///
    /// assert_eq!(ManualClock::starting_at(Nanos(42)).now(), Nanos(42));
    /// ```
    pub fn starting_at(start: Nanos) -> Self {
        Self {
            nanos: AtomicU64::new(start.0),
        }
    }

    /// Moves time forward by `d` (saturating at `u64::MAX` nanoseconds).
    ///
    /// ```
    /// use ratelab::{Clock, ManualClock, Nanos};
    /// use std::time::Duration;
    ///
    /// let clock = ManualClock::new();
    /// clock.advance(Duration::from_nanos(10));
    /// assert_eq!(clock.now(), Nanos(10));
    /// ```
    pub fn advance(&self, d: Duration) {
        let step = Nanos::from_duration(d).0;
        let mut current = self.nanos.load(Ordering::SeqCst);
        loop {
            let next = current.saturating_add(step);
            match self.nanos.compare_exchange_weak(
                current,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// Moves time to `t`. Times earlier than the current time are ignored, so the
    /// clock stays monotonic.
    ///
    /// ```
    /// use ratelab::{Clock, ManualClock, Nanos};
    ///
    /// let clock = ManualClock::new();
    /// clock.set(Nanos(100));
    /// clock.set(Nanos(50)); // ignored
    /// assert_eq!(clock.now(), Nanos(100));
    /// ```
    pub fn set(&self, t: Nanos) {
        self.nanos.fetch_max(t.0, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Nanos {
        Nanos(self.nanos.load(Ordering::SeqCst))
    }
}

/// The real monotonic clock, measured from when it was created.
///
/// This is the only place in `ratelab` that reads system time. It sits behind the
/// default `std-clock` feature; turn that off for targets without a system clock
/// (such as `wasm32-unknown-unknown`) and pass time in through [`ManualClock`].
///
/// ```
/// use ratelab::{Clock, MonotonicClock};
///
/// let clock = MonotonicClock::new();
/// let a = clock.now();
/// let b = clock.now();
/// assert!(b >= a);
/// ```
#[cfg(feature = "std-clock")]
#[derive(Debug, Clone, Copy)]
pub struct MonotonicClock {
    origin: std::time::Instant,
}

#[cfg(feature = "std-clock")]
impl MonotonicClock {
    /// A clock whose origin is now.
    ///
    /// ```
    /// use ratelab::{Clock, MonotonicClock};
    ///
    /// let clock = MonotonicClock::new();
    /// let a = clock.now();
    /// assert!(clock.now() >= a);
    /// ```
    pub fn new() -> Self {
        Self {
            origin: std::time::Instant::now(),
        }
    }
}

#[cfg(feature = "std-clock")]
impl Default for MonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "std-clock")]
impl Clock for MonotonicClock {
    fn now(&self) -> Nanos {
        Nanos::from_duration(self.origin.elapsed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_advances_and_never_goes_back() {
        let clock = ManualClock::new();
        assert_eq!(clock.now(), Nanos::ZERO);
        clock.advance(Duration::from_secs(1));
        assert_eq!(clock.now(), Nanos(1_000_000_000));
        clock.set(Nanos(10));
        assert_eq!(clock.now(), Nanos(1_000_000_000));
    }

    #[test]
    fn manual_clock_advance_saturates() {
        let clock = ManualClock::starting_at(Nanos(u64::MAX - 1));
        clock.advance(Duration::from_secs(1));
        assert_eq!(clock.now(), Nanos(u64::MAX));
    }

    #[test]
    fn shared_handles_see_the_same_time() {
        let clock = Arc::new(ManualClock::new());
        let by_ref: &ManualClock = &clock;
        clock.advance(Duration::from_nanos(7));
        assert_eq!(by_ref.now(), Nanos(7));
        assert_eq!(Clock::now(&clock), Nanos(7));
    }
}
