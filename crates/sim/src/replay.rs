//! Choosing a limit from real traffic: per-key peaks, and the smallest limit at which
//! the exact rule (a sliding window log) blocks no more than a target share of
//! requests. Used by the playground's log replay and the `ratelab` CLI.
//!
//! ```
//! use ratelab::{Cost, Nanos};
//! use sim::Arrival;
//! use sim::replay::{key_peaks, recommend_limit};
//! use std::time::Duration;
//!
//! // Key 0 sends 10 at once; key 1 sends one every 100 ms for 2 s.
//! let mut log: Vec<Arrival> = (0..10).map(|_| Arrival { at: Nanos(0), key: 0, cost: Cost::ONE }).collect();
//! log.extend((0..20).map(|i| Arrival { at: Nanos(i * 100_000_000), key: 1, cost: Cost::ONE }));
//! log.sort_by_key(|a| a.at);
//!
//! let second = Duration::from_secs(1);
//! assert_eq!(key_peaks(&log, second), [10, 10]);
//! assert_eq!(recommend_limit(&log, second, 0.0)?, 10);
//! # Ok::<(), ratelab::ConfigError>(())
//! ```

use crate::{Arrival, run};
use ratelab::{Algorithm, ConfigError, ManualClock, RateLimitConfig};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// The most units each key sent in any `window`-long span, sorted ascending.
///
/// ```
/// use ratelab::{Cost, Nanos};
/// use sim::{replay::key_peaks, Arrival};
/// use std::time::Duration;
///
/// let a = |ms: u64| Arrival { at: Nanos(ms * 1_000_000), key: 0, cost: Cost::ONE };
/// assert_eq!(key_peaks(&[a(0), a(100), a(900), a(1_500)], Duration::from_secs(1)), [3]);
/// ```
pub fn key_peaks(arrivals: &[Arrival], window: Duration) -> Vec<u64> {
    let window_ns = u64::try_from(window.as_nanos()).unwrap_or(u64::MAX).max(1);
    let mut by_key: HashMap<u32, Vec<(u64, u64)>> = HashMap::new();
    for a in arrivals {
        by_key.entry(a.key).or_default().push((a.at.0, a.cost.0));
    }
    let mut peaks: Vec<u64> = by_key
        .values()
        .map(|list| {
            let (mut lo, mut sum, mut best) = (0usize, 0u64, 0u64);
            for &(at, cost) in list {
                sum += cost;
                while at - list[lo].0 >= window_ns {
                    sum -= list[lo].1;
                    lo += 1;
                }
                best = best.max(sum);
            }
            best
        })
        .collect();
    peaks.sort_unstable();
    peaks
}

/// Nearest-rank quantile `q` (0 to 1) of sorted values; 0 if empty.
///
/// ```
/// assert_eq!(sim::replay::quantile(&[1, 2, 3, 4], 0.5), 2);
/// assert_eq!(sim::replay::quantile(&[], 0.5), 0);
/// ```
pub fn quantile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// Requests the exact rule blocks under `config`, as a % of all requests.
///
/// ```
/// use ratelab::{Cost, Nanos, RateLimitConfig};
/// use sim::{replay::exact_blocked_pct, Arrival};
///
/// let burst: Vec<Arrival> = (0..4).map(|_| Arrival { at: Nanos(0), key: 0, cost: Cost::ONE }).collect();
/// assert_eq!(exact_blocked_pct(&burst, RateLimitConfig::per_second(3)?), 25.0);
/// # Ok::<(), ratelab::ConfigError>(())
/// ```
pub fn exact_blocked_pct(arrivals: &[Arrival], config: RateLimitConfig) -> f64 {
    let clock = Arc::new(ManualClock::new());
    let exact = Algorithm::SlidingWindow.build(config, Arc::clone(&clock));
    let trace = run(&exact, &clock, arrivals);
    trace.denied() as f64 * 100.0 / arrivals.len().max(1) as f64
}

/// The smallest per-key limit per `window` at which the exact rule blocks at most
/// `max_blocked_pct` % of requests. Binary search between 1 and the busiest key's peak.
pub fn recommend_limit(
    arrivals: &[Arrival],
    window: Duration,
    max_blocked_pct: f64,
) -> Result<u64, ConfigError> {
    RateLimitConfig::new(1, window)?;
    let mut hi = key_peaks(arrivals, window)
        .last()
        .copied()
        .unwrap_or(1)
        .max(1);
    let mut lo = 1u64;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if exact_blocked_pct(arrivals, RateLimitConfig::new(mid, window)?) <= max_blocked_pct {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(lo)
}
