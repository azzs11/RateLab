//! Rate limit response headers.
//!
//! Follows the IETF HTTPAPI draft "RateLimit header fields for HTTP"
//! (draft-ietf-httpapi-ratelimit-headers): two structured-field lists, each item a
//! policy name with parameters.
//!
//! ```text
//! RateLimit-Policy: "orders";q=100;w=60     quota q per window w (seconds)
//! RateLimit: "orders";r=42;t=17             r units remaining, full again in t seconds
//! Retry-After: 3                            on 429, seconds until a retry can succeed
//! ```
//!
//! The draft counts in whole seconds, so sub-second values are rounded **up**: a client
//! that waits `t` or `Retry-After` seconds never retries too early.

use ratelab::{Decision, RateLimitConfig};
use std::time::Duration;

/// Whole seconds, rounded up.
pub fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

/// `RateLimit-Policy` value for `policy` with `config`.
///
/// ```
/// use ratelab::RateLimitConfig;
/// use server::headers::policy_header;
///
/// let config = RateLimitConfig::per_minute(100).unwrap();
/// assert_eq!(policy_header("orders", config), r#""orders";q=100;w=60"#);
/// ```
pub fn policy_header(policy: &str, config: RateLimitConfig) -> String {
    format!(
        "\"{policy}\";q={};w={}",
        config.limit(),
        ceil_secs(config.window())
    )
}

/// `RateLimit` value for `policy` after `decision`.
///
/// ```
/// use ratelab::{Decision, Tokens};
/// use server::headers::ratelimit_header;
/// use std::time::Duration;
///
/// let d = Decision::allow(Tokens(42), Duration::from_millis(16_200));
/// assert_eq!(ratelimit_header("orders", &d), r#""orders";r=42;t=17"#);
/// ```
pub fn ratelimit_header(policy: &str, decision: &Decision) -> String {
    format!(
        "\"{policy}\";r={};t={}",
        decision.remaining.0,
        ceil_secs(decision.reset_after)
    )
}

/// `Retry-After` value (seconds) for a denied decision, at least 1. `None` when the
/// request can never succeed (cost above the limit).
///
/// ```
/// use ratelab::{Decision, Tokens};
/// use server::headers::retry_after_header;
/// use std::time::Duration;
///
/// let d = Decision::deny(Tokens(0), Duration::from_secs(1), Some(Duration::from_millis(250)));
/// assert_eq!(retry_after_header(&d), Some(1));
/// ```
pub fn retry_after_header(decision: &Decision) -> Option<u64> {
    decision.retry_after.map(|d| ceil_secs(d).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratelab::Tokens;

    #[test]
    fn rounds_up_never_down() {
        assert_eq!(ceil_secs(Duration::ZERO), 0);
        assert_eq!(ceil_secs(Duration::from_nanos(1)), 1);
        assert_eq!(ceil_secs(Duration::from_secs(2)), 2);
        assert_eq!(ceil_secs(Duration::from_millis(2001)), 3);
    }

    #[test]
    fn sub_second_window_is_reported_as_one_second() {
        let config = RateLimitConfig::new(5, Duration::from_millis(500)).unwrap();
        assert_eq!(policy_header("p", config), r#""p";q=5;w=1"#);
    }

    #[test]
    fn retry_after_is_absent_when_the_request_can_never_succeed() {
        let d = Decision::deny(Tokens(0), Duration::from_secs(1), None);
        assert_eq!(retry_after_header(&d), None);
        let d = Decision::deny(Tokens(0), Duration::from_secs(1), Some(Duration::ZERO));
        assert_eq!(retry_after_header(&d), Some(1));
    }
}
