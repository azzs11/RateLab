//! Bytes per key, measured by a counting global allocator in the `memory` binary.

use drl_core::{Algorithm, Cost, ManualClock, RateLimitConfig};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Heap bytes per key for one algorithm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRow {
    /// Algorithm name.
    pub algorithm: String,
    /// Keys stored.
    pub keys: usize,
    /// The per-key limit.
    pub limit: u64,
    /// After one request per key.
    pub bytes_per_key_one_request: f64,
    /// After `limit` requests per key (a full window).
    pub bytes_per_key_full_window: f64,
}

/// Measures every algorithm. `allocated` returns the bytes currently allocated.
pub fn measure(keys: usize, limit: u64, allocated: impl Fn() -> usize) -> Vec<MemoryRow> {
    // A long window so nothing expires or refills while the keys fill up.
    let config = RateLimitConfig::new(limit, Duration::from_secs(3600)).expect("valid config");
    let names: Vec<String> = (0..keys).map(|i| format!("user{i:07}")).collect();
    Algorithm::ALL
        .iter()
        .map(|&algorithm| {
            let clock = Arc::new(ManualClock::new());
            let before = allocated();
            let limiter = algorithm.build(config, Arc::clone(&clock));
            for key in &names {
                limiter.check(key, Cost::ONE);
            }
            let one = allocated().saturating_sub(before);
            for _ in 1..limit {
                for key in &names {
                    limiter.check(key, Cost::ONE);
                }
            }
            let full = allocated().saturating_sub(before);
            drop(limiter);
            MemoryRow {
                algorithm: algorithm.name().to_owned(),
                keys,
                limit,
                bytes_per_key_one_request: one as f64 / keys as f64,
                bytes_per_key_full_window: full as f64 / keys as f64,
            }
        })
        .collect()
}
