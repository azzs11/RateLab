//! WebAssembly bindings for `drl-core` and `sim`, used by the browser playground.
//!
//! - [`Limiter`]: one live limiter, time passed in from JavaScript;
//! - [`traffic::Traffic`]: seeded traffic patterns, or arrays from a user's log;
//! - [`lab::LabRun`]: one algorithm over some traffic, with its state over time;
//! - [`cluster::ClusterView`]: the multi-node simulator with any strategy;
//! - [`overload::OverloadRun`]: concurrency limits in front of a backend with limited
//!   capacity;
//! - [`replay::ReplayReport`]: a log replayed through everything, plus a limit
//!   recommendation.
//!
//! Everything is the same Rust code the library, the tests and RESULTS.md use.
//!
//! The browser has no system clock that `wasm32-unknown-unknown` can read, so every
//! call takes the current time from JavaScript (`performance.now()` in milliseconds).
//! Time never moves backwards: an older `now_ms` is treated as "no time passed".
//!
//! ```js
//! import init, { Limiter } from "./pkg/wasm.js";
//! await init();
//! const limiter = new Limiter("gcra", 10, 1000);
//! const d = limiter.check("alice", 1, performance.now());
//! console.log(d.allowed, d.remaining, d.retry_after_ms);
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod cluster;
pub mod lab;
pub mod overload;
pub mod replay;
pub mod traffic;

use drl_core::{Algorithm, Cost, ManualClock, Nanos, RateLimitConfig, RateLimiter};
use std::sync::Arc;
use std::time::Duration;
use wasm_bindgen::prelude::*;

/// A rate limiter for use from JavaScript.
#[wasm_bindgen]
pub struct Limiter {
    inner: Box<dyn RateLimiter>,
    clock: Arc<ManualClock>,
}

/// The result of [`Limiter::check`], in JavaScript-friendly units.
#[wasm_bindgen]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheckResult {
    /// Whether the request may proceed.
    pub allowed: bool,
    /// Units still available to the key after this decision.
    pub remaining: u32,
    /// Milliseconds until the key is back to its full quota.
    pub reset_after_ms: f64,
    /// For a denied request, milliseconds until the same request could succeed;
    /// `undefined` if it was allowed or can never succeed.
    pub retry_after_ms: Option<f64>,
}

#[wasm_bindgen]
impl Limiter {
    /// Creates a limiter. `algorithm` is one of `token_bucket`, `leaky_bucket`,
    /// `fixed_window`, `sliding_window`, `gcra`. Throws on an unknown algorithm or an
    /// invalid limit/window.
    #[wasm_bindgen(constructor)]
    pub fn new(algorithm: &str, limit: u32, window_ms: f64) -> Result<Limiter, JsError> {
        Limiter::build(algorithm, limit, window_ms).map_err(|e| JsError::new(&e))
    }

    /// Checks and charges `cost` units for `key` at time `now_ms`.
    pub fn check(&self, key: &str, cost: u32, now_ms: f64) -> CheckResult {
        self.clock.set(ms_to_nanos(now_ms));
        let d = self.inner.check(key, Cost(u64::from(cost)));
        CheckResult {
            allowed: d.allowed,
            remaining: u32::try_from(d.remaining.0).unwrap_or(u32::MAX),
            reset_after_ms: to_ms(d.reset_after),
            retry_after_ms: d.retry_after.map(to_ms),
        }
    }

    /// Forgets all state for `key`.
    pub fn reset(&self, key: &str) {
        self.inner.reset(key);
    }

    /// Names of every available algorithm.
    pub fn algorithms() -> Vec<String> {
        Algorithm::ALL.iter().map(|a| a.name().to_owned()).collect()
    }
}

impl Limiter {
    /// The constructor's logic, with a plain error so it can be tested natively.
    fn build(algorithm: &str, limit: u32, window_ms: f64) -> Result<Limiter, String> {
        let algorithm: Algorithm = algorithm.parse().map_err(|e| format!("{e}"))?;
        if !(window_ms.is_finite() && window_ms > 0.0) {
            return Err("window_ms must be a positive number".to_owned());
        }
        let window = Duration::from_nanos(ms_to_nanos(window_ms).0);
        let config = RateLimitConfig::new(u64::from(limit), window).map_err(|e| e.to_string())?;
        let clock = Arc::new(ManualClock::new());
        Ok(Limiter {
            inner: algorithm.build(config, Arc::clone(&clock)),
            clock,
        })
    }
}

/// Milliseconds (as from `performance.now()`) to `Nanos`. Negative or NaN becomes 0;
/// huge values saturate.
fn ms_to_nanos(ms: f64) -> Nanos {
    if ms.is_nan() || ms <= 0.0 {
        return Nanos::ZERO;
    }
    // `as` saturates for floats, so this cannot overflow.
    Nanos((ms * 1_000_000.0).round() as u64)
}

fn to_ms(d: Duration) -> f64 {
    d.as_nanos() as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_every_algorithm() {
        for name in Limiter::algorithms() {
            assert!(Limiter::build(&name, 5, 1000.0).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Limiter::build("nope", 5, 1000.0).is_err());
        assert!(Limiter::build("gcra", 0, 1000.0).is_err());
        assert!(Limiter::build("gcra", 5, 0.0).is_err());
        assert!(Limiter::build("gcra", 5, f64::NAN).is_err());
    }

    #[test]
    fn time_comes_from_the_caller() {
        let l = Limiter::build("token_bucket", 2, 1000.0).unwrap();
        assert!(l.check("k", 1, 0.0).allowed);
        assert!(l.check("k", 1, 0.0).allowed);
        let denied = l.check("k", 1, 0.0);
        assert!(!denied.allowed);
        assert_eq!(denied.retry_after_ms, Some(500.0));

        assert!(!l.check("k", 1, 499.0).allowed);
        assert!(l.check("k", 1, 500.0).allowed);
    }

    #[test]
    fn time_never_moves_backwards() {
        let l = Limiter::build("fixed_window", 1, 1000.0).unwrap();
        assert!(l.check("k", 1, 1500.0).allowed);
        // An older timestamp must not reopen an earlier window.
        assert!(!l.check("k", 1, 200.0).allowed);
    }

    #[test]
    fn ms_conversion_handles_edge_cases() {
        assert_eq!(ms_to_nanos(-5.0), Nanos::ZERO);
        assert_eq!(ms_to_nanos(f64::NAN), Nanos::ZERO);
        assert_eq!(ms_to_nanos(1.5), Nanos(1_500_000));
        assert_eq!(ms_to_nanos(f64::INFINITY), Nanos(u64::MAX));
    }

    #[test]
    fn matches_the_native_library() {
        // The wrapper must not change decisions: compare with drl-core directly.
        let wasm = Limiter::build("sliding_window", 3, 100.0).unwrap();
        let clock = Arc::new(ManualClock::new());
        let native = Algorithm::SlidingWindow.build(
            RateLimitConfig::new(3, Duration::from_millis(100)).unwrap(),
            Arc::clone(&clock),
        );
        let mut t = 0.0;
        for i in 0..500u32 {
            t += f64::from(i % 7) * 3.0;
            clock.set(ms_to_nanos(t));
            let n = native.check("k", Cost(u64::from(i % 3)));
            let w = wasm.check("k", i % 3, t);
            assert_eq!(w.allowed, n.allowed, "step {i}");
            assert_eq!(u64::from(w.remaining), n.remaining.0, "step {i}");
        }
    }
}
