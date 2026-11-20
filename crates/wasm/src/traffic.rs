//! Traffic for the playground: seeded patterns, or arrays from a user's log.

use ratelab::{Cost, Nanos, RateLimitConfig};
use sim::Arrival;
use std::time::Duration;
use wasm_bindgen::prelude::*;

/// Every pattern [`Traffic::generate`] accepts.
pub use sim::patterns::PATTERNS;

/// Requests over time: what a limiter or a cluster is fed.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq)]
pub struct Traffic {
    pub(crate) arrivals: Vec<Arrival>,
}

pub(crate) fn ms_to_nanos(ms: f64) -> u64 {
    if ms.is_nan() || ms <= 0.0 {
        0
    } else {
        (ms * 1e6).round() as u64
    }
}

pub(crate) fn config(limit: u32, window_ms: f64) -> Result<RateLimitConfig, String> {
    if !(window_ms.is_finite() && window_ms > 0.0) {
        return Err("window_ms must be a positive number".to_owned());
    }
    RateLimitConfig::new(
        u64::from(limit),
        Duration::from_nanos(ms_to_nanos(window_ms)),
    )
    .map_err(|e| e.to_string())
}

impl Traffic {
    /// Builds a pattern. `load` is the offered rate per key as a multiple of the limit.
    pub fn build(
        pattern: &str,
        seed: u64,
        secs: f64,
        limit: u32,
        window_ms: f64,
        keys: u32,
        load: f64,
    ) -> Result<Traffic, String> {
        let config = config(limit, window_ms)?;
        if !(secs.is_finite() && secs > 0.0 && secs <= 600.0) {
            return Err("secs must be between 0 and 600".to_owned());
        }
        let d = Duration::from_nanos(ms_to_nanos(secs * 1_000.0));
        let arrivals = sim::patterns::generate(pattern, seed, d, config, keys, load)?;
        Ok(Traffic { arrivals })
    }

    /// Builds traffic from parallel arrays, sorting by time.
    pub fn from_parts(at_ms: &[f64], keys: &[u32], costs: &[u32]) -> Result<Traffic, String> {
        if at_ms.len() != keys.len() || (!costs.is_empty() && costs.len() != at_ms.len()) {
            return Err("at_ms, keys and costs must have the same length".to_owned());
        }
        let start = at_ms.iter().copied().fold(f64::INFINITY, f64::min);
        let mut arrivals: Vec<Arrival> = at_ms
            .iter()
            .zip(keys)
            .enumerate()
            .map(|(i, (&t, &key))| Arrival {
                at: Nanos(ms_to_nanos(t - start)),
                key,
                cost: Cost(u64::from(costs.get(i).copied().unwrap_or(1))),
            })
            .collect();
        arrivals.sort_by_key(|a| a.at);
        Ok(Traffic { arrivals })
    }
}

#[wasm_bindgen]
impl Traffic {
    /// A seeded traffic pattern (see [`patterns`]). Throws on bad input.
    pub fn generate(
        pattern: &str,
        seed: u32,
        secs: f64,
        limit: u32,
        window_ms: f64,
        keys: u32,
        load: f64,
    ) -> Result<Traffic, JsError> {
        Traffic::build(pattern, u64::from(seed), secs, limit, window_ms, keys, load)
            .map_err(|e| JsError::new(&e))
    }

    /// Traffic from a user's log: times in ms (any origin), key ids, optional costs.
    #[wasm_bindgen(js_name = fromArrays)]
    pub fn from_arrays(at_ms: &[f64], keys: &[u32], costs: &[u32]) -> Result<Traffic, JsError> {
        Traffic::from_parts(at_ms, keys, costs).map_err(|e| JsError::new(&e))
    }

    /// Number of requests.
    pub fn len(&self) -> usize {
        self.arrivals.len()
    }

    /// Whether there are no requests.
    #[wasm_bindgen(js_name = isEmpty)]
    pub fn is_empty(&self) -> bool {
        self.arrivals.is_empty()
    }

    /// Arrival times in ms from the first request.
    #[wasm_bindgen(js_name = atMs)]
    pub fn at_ms(&self) -> Vec<f64> {
        self.arrivals.iter().map(|a| a.at.0 as f64 / 1e6).collect()
    }

    /// Key of each request.
    pub fn keys(&self) -> Vec<u32> {
        self.arrivals.iter().map(|a| a.key).collect()
    }

    /// Time of the last request in ms.
    #[wasm_bindgen(js_name = durationMs)]
    pub fn duration_ms(&self) -> f64 {
        self.arrivals.last().map_or(0.0, |a| a.at.0 as f64 / 1e6)
    }
}

/// Names of every traffic pattern.
#[wasm_bindgen]
pub fn patterns() -> Vec<String> {
    PATTERNS.iter().map(|&p| p.to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pattern_builds_and_is_sorted() {
        for p in PATTERNS {
            let t = Traffic::build(p, 1, 3.0, 10, 1_000.0, 2, 2.0).unwrap();
            assert!(!t.is_empty(), "{p}");
            assert!(t.arrivals.windows(2).all(|w| w[0].at <= w[1].at), "{p}");
        }
    }

    #[test]
    fn load_sets_the_rate() {
        let t = Traffic::build("steady", 3, 20.0, 50, 1_000.0, 2, 2.0).unwrap();
        // 50/s × 2 × 2 keys × 20 s = 4,000 requests on average.
        assert!((3_800..4_200).contains(&t.len()), "{}", t.len());
    }

    #[test]
    fn attack_repeats_every_two_windows() {
        let t = Traffic::build("boundary_attack", 0, 5.0, 4, 1_000.0, 1, 1.0).unwrap();
        // Edges at 1 s and 3 s fit in 5 s: 2 attacks × 2 waves × 4 requests.
        assert_eq!(t.len(), 16);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(Traffic::build("nope", 1, 1.0, 1, 1.0, 1, 1.0).is_err());
        assert!(Traffic::build("steady", 1, 0.0, 1, 1.0, 1, 1.0).is_err());
        assert!(Traffic::build("steady", 1, 1.0, 0, 1.0, 1, 1.0).is_err());
        assert!(Traffic::build("steady", 1, 1.0, 1, 1.0, 1, f64::NAN).is_err());
    }

    #[test]
    fn log_arrays_are_rebased_and_sorted() {
        let t = Traffic::from_parts(&[1_005.0, 1_000.0], &[7, 8], &[]).unwrap();
        assert_eq!(t.at_ms(), [0.0, 5.0]);
        assert_eq!(t.keys(), [8, 7]);
        assert!(Traffic::from_parts(&[1.0], &[1, 2], &[]).is_err());
    }
}
