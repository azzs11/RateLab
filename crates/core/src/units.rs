//! Unit newtypes, so a count can never be passed where a time is expected.

use std::time::Duration;

/// A point in time, in nanoseconds since the origin of some [`Clock`](crate::Clock).
///
/// Only differences between two `Nanos` from the same clock are meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Nanos(pub u64);

impl Nanos {
    /// The clock origin.
    pub const ZERO: Nanos = Nanos(0);

    /// Converts a duration since the clock origin into `Nanos`, saturating at `u64::MAX`.
    ///
    /// ```
    /// use ratelab::Nanos;
    /// use std::time::Duration;
    ///
    /// assert_eq!(Nanos::from_duration(Duration::from_micros(3)), Nanos(3_000));
    /// ```
    pub fn from_duration(d: Duration) -> Self {
        Nanos(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
    }

    /// This instant moved forward by `d`, saturating at `u64::MAX`.
    ///
    /// ```
    /// use ratelab::Nanos;
    /// use std::time::Duration;
    ///
    /// assert_eq!(Nanos(5).saturating_add(Duration::from_nanos(2)), Nanos(7));
    /// ```
    pub fn saturating_add(self, d: Duration) -> Self {
        Nanos(self.0.saturating_add(Nanos::from_duration(d).0))
    }

    /// Time elapsed since `earlier`, or zero if `earlier` is later than `self`.
    ///
    /// ```
    /// use ratelab::Nanos;
    /// use std::time::Duration;
    ///
    /// assert_eq!(Nanos(10).saturating_since(Nanos(4)), Duration::from_nanos(6));
    /// assert_eq!(Nanos(4).saturating_since(Nanos(10)), Duration::ZERO);
    /// ```
    pub fn saturating_since(self, earlier: Nanos) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }
}

/// How many quota units a single request consumes (1 for plain request counting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cost(pub u64);

impl Cost {
    /// The cost of one ordinary request.
    pub const ONE: Cost = Cost(1);
}

impl Default for Cost {
    fn default() -> Self {
        Cost::ONE
    }
}

/// A number of quota units, for example how many remain for a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Tokens(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_duration_saturates() {
        assert_eq!(Nanos::from_duration(Duration::MAX), Nanos(u64::MAX));
    }

    #[test]
    fn saturating_add_saturates() {
        assert_eq!(
            Nanos(u64::MAX - 1).saturating_add(Duration::from_secs(1)),
            Nanos(u64::MAX)
        );
    }

    #[test]
    fn default_cost_is_one() {
        assert_eq!(Cost::default(), Cost::ONE);
    }
}
