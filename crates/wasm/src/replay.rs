//! Bring-your-own-logs: replay real traffic through every algorithm and strategy, and
//! recommend a limit. Everything runs in the browser; the log never leaves it.

use crate::cluster::{ClusterOptions, ClusterView, STRATEGIES};
use crate::traffic::{Traffic, config};
use drl_core::{Algorithm, ManualClock};
use sim::Arrival;
use std::collections::HashMap;
use std::sync::Arc;
use wasm_bindgen::prelude::*;

/// The most requests each key sent in any window, sorted ascending.
pub fn key_peaks(arrivals: &[Arrival], window_ns: u64) -> Vec<u64> {
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

fn quantile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// Requests the exact rule blocks at `limit`, as % of all requests.
fn exact_blocked_pct(arrivals: &[Arrival], limit: u64, window_ms: f64) -> Result<f64, String> {
    let config = config(u32::try_from(limit).unwrap_or(u32::MAX), window_ms)?;
    let clock = Arc::new(ManualClock::new());
    let exact = Algorithm::SlidingWindow.build(config, Arc::clone(&clock));
    let trace = sim::run(&exact, &clock, arrivals);
    Ok(trace.denied() as f64 * 100.0 / arrivals.len().max(1) as f64)
}

/// The smallest per-key limit for which the exact rule blocks at most
/// `max_blocked_pct` % of requests. Binary search between 1 and the busiest key's peak.
pub fn recommend_limit(
    arrivals: &[Arrival],
    window_ms: f64,
    max_blocked_pct: f64,
) -> Result<u64, String> {
    let window_ns = config(1, window_ms)?.window_nanos();
    let mut hi = key_peaks(arrivals, window_ns)
        .last()
        .copied()
        .unwrap_or(1)
        .max(1);
    let mut lo = 1u64;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if exact_blocked_pct(arrivals, mid, window_ms)? <= max_blocked_pct {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(lo)
}

/// What every algorithm and strategy would have done with a log.
#[wasm_bindgen(getter_with_clone)]
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayReport {
    /// Requests in the log.
    pub requests: u32,
    /// Distinct keys.
    pub keys: u32,
    /// Time from first to last request, ms.
    pub duration_ms: f64,
    /// Per-key peak requests in one window: median.
    pub peak_p50: u32,
    /// Per-key peak: 90th percentile.
    pub peak_p90: u32,
    /// Per-key peak: 99th percentile.
    pub peak_p99: u32,
    /// Per-key peak: busiest key.
    pub peak_max: u32,
    /// Algorithm names.
    pub algorithms: Vec<String>,
    /// Requests blocked by each algorithm, %.
    pub algorithm_blocked_pct: Vec<f64>,
    /// Most units one key got in one window under each algorithm.
    pub algorithm_worst_window: Vec<u32>,
    /// Keys that had at least one request blocked, per algorithm.
    pub algorithm_keys_limited: Vec<u32>,
    /// Strategy names.
    pub strategies: Vec<String>,
    /// Admission error of each strategy against one exact bucket, %.
    pub strategy_error_pct: Vec<f64>,
    /// Store calls per 1,000 decisions, per strategy.
    pub strategy_calls_per_1k: Vec<f64>,
    /// Decision wait p99 per strategy, µs.
    pub strategy_p99_us: Vec<f64>,
}

impl ReplayReport {
    /// Replays `traffic` at `limit` per `window_ms`, strategies on `nodes` nodes.
    pub fn build(
        traffic: &Traffic,
        limit: u32,
        window_ms: f64,
        nodes: u32,
    ) -> Result<ReplayReport, String> {
        let config = config(limit, window_ms)?;
        if traffic.is_empty() {
            return Err("the log has no requests".to_owned());
        }
        let peaks = key_peaks(&traffic.arrivals, config.window_nanos());
        let as_u32 = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
        let mut report = ReplayReport {
            requests: as_u32(traffic.len() as u64),
            keys: as_u32(peaks.len() as u64),
            duration_ms: traffic.duration_ms(),
            peak_p50: as_u32(quantile(&peaks, 0.5)),
            peak_p90: as_u32(quantile(&peaks, 0.9)),
            peak_p99: as_u32(quantile(&peaks, 0.99)),
            peak_max: as_u32(peaks.last().copied().unwrap_or(0)),
            algorithms: Vec::new(),
            algorithm_blocked_pct: Vec::new(),
            algorithm_worst_window: Vec::new(),
            algorithm_keys_limited: Vec::new(),
            strategies: Vec::new(),
            strategy_error_pct: Vec::new(),
            strategy_calls_per_1k: Vec::new(),
            strategy_p99_us: Vec::new(),
        };
        for algorithm in Algorithm::ALL {
            let clock = Arc::new(ManualClock::new());
            let limiter = algorithm.build(config, Arc::clone(&clock));
            let trace = sim::run(&limiter, &clock, &traffic.arrivals);
            let mut limited: Vec<u32> = trace
                .steps
                .iter()
                .filter(|s| !s.decision.allowed)
                .map(|s| s.arrival.key)
                .collect();
            limited.sort_unstable();
            limited.dedup();
            report.algorithms.push(algorithm.name().to_owned());
            report
                .algorithm_blocked_pct
                .push(trace.denied() as f64 * 100.0 / traffic.len() as f64);
            report
                .algorithm_worst_window
                .push(as_u32(trace.max_admitted_in_window(config.window())));
            report
                .algorithm_keys_limited
                .push(as_u32(limited.len() as u64));
        }
        for s in STRATEGIES {
            let options = ClusterOptions {
                strategy: s.to_owned(),
                nodes,
                limit,
                window_ms,
                sample_ms: window_ms.max(1.0) * 10.0,
                ..ClusterOptions::default()
            };
            let view = ClusterView::build(&options, traffic)?;
            report.strategies.push(s.to_owned());
            report
                .strategy_error_pct
                .push(view.over_pct() + view.under_pct());
            report.strategy_calls_per_1k.push(view.calls_per_1k());
            report.strategy_p99_us.push(view.p99_us());
        }
        Ok(report)
    }
}

#[wasm_bindgen]
impl ReplayReport {
    /// Replays a log at `limit` per `window_ms`; strategies run on `nodes` nodes with
    /// requests spread evenly. Throws on bad input.
    pub fn run(
        traffic: &Traffic,
        limit: u32,
        window_ms: f64,
        nodes: u32,
    ) -> Result<ReplayReport, JsError> {
        ReplayReport::build(traffic, limit, window_ms, nodes).map_err(|e| JsError::new(&e))
    }
}

/// The smallest per-key limit per `window_ms` at which the exact rule blocks at most
/// `max_blocked_pct` % of the log's requests.
#[wasm_bindgen(js_name = recommendLimit)]
pub fn recommend_limit_js(
    traffic: &Traffic,
    window_ms: f64,
    max_blocked_pct: f64,
) -> Result<u32, JsError> {
    recommend_limit(&traffic.arrivals, window_ms, max_blocked_pct)
        .map(|l| u32::try_from(l).unwrap_or(u32::MAX))
        .map_err(|e| JsError::new(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log() -> Traffic {
        // Key 0: a burst of 10 at t=0; key 1: one request per 100 ms for 2 s.
        let mut at = vec![0.0; 10];
        let mut keys = vec![0; 10];
        for i in 0..20 {
            at.push(f64::from(i) * 100.0);
            keys.push(1);
        }
        Traffic::from_parts(&at, &keys, &[]).unwrap()
    }

    #[test]
    fn peaks_are_per_key_maxima() {
        let t = log();
        assert_eq!(key_peaks(&t.arrivals, 1_000_000_000), [10, 10]);
        assert_eq!(key_peaks(&t.arrivals, 250_000_000), [3, 10]);
    }

    #[test]
    fn recommendation_meets_the_target_exactly() {
        let t = log();
        assert_eq!(recommend_limit(&t.arrivals, 1_000.0, 0.0).unwrap(), 10);
        // With 10% allowed to be blocked, the answer is the smallest limit that meets it.
        let l = recommend_limit(&t.arrivals, 1_000.0, 10.0).unwrap();
        assert!(exact_blocked_pct(&t.arrivals, l, 1_000.0).unwrap() <= 10.0);
        assert!(exact_blocked_pct(&t.arrivals, l - 1, 1_000.0).unwrap() > 10.0);
    }

    #[test]
    fn report_covers_every_algorithm_and_strategy() {
        let r = ReplayReport::build(&log(), 5, 1_000.0, 3).unwrap();
        assert_eq!(r.requests, 30);
        assert_eq!(r.keys, 2);
        assert_eq!(r.algorithms.len(), 5);
        assert_eq!(r.strategies.len(), 5);
        let fixed = r
            .algorithms
            .iter()
            .position(|a| a == "fixed_window")
            .unwrap();
        assert_eq!(r.algorithm_keys_limited[fixed], 2);
        assert!(ReplayReport::build(&Traffic { arrivals: vec![] }, 5, 1_000.0, 3).is_err());
    }
}
