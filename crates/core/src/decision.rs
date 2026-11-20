//! The result of asking a limiter whether a request may proceed.

use crate::units::Tokens;
use std::time::Duration;

/// What a limiter decided for one request, plus what a client needs to back off well.
///
/// The fields map directly onto the IETF `RateLimit` and `Retry-After` response headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// Whether the request may proceed. If true, its cost has been charged.
    pub allowed: bool,
    /// Quota units still available to this key right after this decision.
    pub remaining: Tokens,
    /// Time until this key is back to its full quota, if it receives no more requests.
    pub reset_after: Duration,
    /// For a denied request: how long to wait before the same request could be
    /// admitted, assuming no other traffic for this key. `None` if the request was
    /// allowed, or if it can never be admitted (its cost exceeds the limit).
    pub retry_after: Option<Duration>,
}

impl Decision {
    /// An admitted request.
    ///
    /// ```
    /// use drl_core::{Decision, Tokens};
    /// use std::time::Duration;
    ///
    /// let d = Decision::allow(Tokens(4), Duration::from_secs(1));
    /// assert!(d.allowed);
    /// assert_eq!(d.retry_after, None);
    /// ```
    pub fn allow(remaining: Tokens, reset_after: Duration) -> Self {
        Self {
            allowed: true,
            remaining,
            reset_after,
            retry_after: None,
        }
    }

    /// A rejected request.
    ///
    /// ```
    /// use drl_core::{Decision, Tokens};
    /// use std::time::Duration;
    ///
    /// let d = Decision::deny(Tokens(0), Duration::from_secs(1), Some(Duration::from_millis(200)));
    /// assert!(!d.allowed);
    /// ```
    pub fn deny(remaining: Tokens, reset_after: Duration, retry_after: Option<Duration>) -> Self {
        Self {
            allowed: false,
            remaining,
            reset_after,
            retry_after,
        }
    }
}
