//! Every algorithm and strategy on a real request log (see [`crate::traces`]).
//!
//! The limit is not picked by hand: it is the smallest per-client limit per window for
//! which the exact rule (the sliding window log) blocks at most [`TARGET_BLOCKED_PCT`]
//! of the log's requests, found by binary search, like the playground's recommender.

use crate::oracle::{ExactOracle, compare};
use crate::strategies::NETWORK;
use crate::traces::TraceInfo;
use drl_core::strategies::Strategy;
use drl_core::{Algorithm, FailurePolicy, ManualClock, RateLimitConfig};
use serde::{Deserialize, Serialize};
use sim::Arrival;
use sim::cluster::{ClusterConfig, place, run_cluster};
use sim::metrics;
use std::sync::Arc;
use std::time::Duration;

/// Share of requests the chosen limit may block under the exact rule, %.
pub const TARGET_BLOCKED_PCT: f64 = 1.0;
/// The window: one minute.
pub const WINDOW_SECS: u64 = 60;

fn config(limit: u64) -> RateLimitConfig {
    RateLimitConfig::new(limit, Duration::from_secs(WINDOW_SECS)).expect("limit is at least 1")
}

fn exact_blocked_pct(arrivals: &[Arrival], limit: u64) -> f64 {
    let clock = Arc::new(ManualClock::new());
    let exact = Algorithm::SlidingWindow.build(config(limit), Arc::clone(&clock));
    let trace = sim::run(&exact, &clock, arrivals);
    trace.denied() as f64 * 100.0 / arrivals.len().max(1) as f64
}

/// The smallest limit for which the exact rule blocks at most `target` % of requests.
pub fn choose_limit(arrivals: &[Arrival], target: f64) -> u64 {
    // Upper bound: the busiest client's count in its busiest window blocks nothing.
    let all_allowed = sim::Trace {
        steps: arrivals
            .iter()
            .map(|&arrival| sim::Step {
                arrival,
                decision: drl_core::Decision::allow(drl_core::Tokens(0), Duration::ZERO),
            })
            .collect(),
    };
    let mut hi = all_allowed
        .max_admitted_in_window(Duration::from_secs(WINDOW_SECS))
        .max(1);
    let mut lo = 1;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if exact_blocked_pct(arrivals, mid) <= target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// One algorithm on the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlgorithmRow {
    /// Algorithm name.
    pub algorithm: String,
    /// Requests blocked, %.
    pub blocked_pct: f64,
    /// Allowed by the algorithm, denied by the exact oracle, % of requests.
    pub over_pct: f64,
    /// Denied by the algorithm, allowed by the oracle, % of requests.
    pub under_pct: f64,
    /// Most units one client got in one window / limit.
    pub worst_window_ratio: f64,
}

/// One coordination strategy on the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRow {
    /// Strategy name.
    pub strategy: String,
    /// Nodes; each request goes to a node chosen at random, as a load balancer would.
    pub nodes: usize,
    /// Admission error against one coordinated token bucket, % of its admitted units.
    pub error_pct: f64,
    /// Over-admission part, %.
    pub over_pct: f64,
    /// Under-admission part, %.
    pub under_pct: f64,
    /// Store calls per 1,000 decisions.
    pub calls_per_1k: f64,
    /// Decision wait p99, µs.
    pub p99_us: f64,
}

/// `results/traces.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceResults {
    /// File format version.
    pub schema: u32,
    /// Where it ran.
    pub machine: crate::machine::Machine,
    /// The trace (aggregates only).
    pub trace: TraceInfo,
    /// Window, seconds.
    pub window_secs: u64,
    /// The chosen per-client limit per window.
    pub limit: u64,
    /// The blocking target the limit was chosen for, %.
    pub target_blocked_pct: f64,
    /// Every algorithm.
    pub algorithms: Vec<AlgorithmRow>,
    /// Every strategy on each cluster size.
    pub strategies: Vec<StrategyRow>,
}

/// Replays `arrivals` through every algorithm and strategy.
pub fn replay(
    arrivals: &[Arrival],
    nodes: &[usize],
    mut progress: impl FnMut(&str),
) -> (u64, Vec<AlgorithmRow>, Vec<StrategyRow>) {
    progress("choosing the limit");
    let limit = choose_limit(arrivals, TARGET_BLOCKED_PCT);
    let cfg = config(limit);
    let window_ns = cfg.window_nanos();

    progress("algorithms");
    let oracle = ExactOracle::decide_all(limit, window_ns, arrivals);
    let algorithms = Algorithm::ALL
        .iter()
        .map(|&a| {
            let clock = Arc::new(ManualClock::new());
            let limiter = a.build(cfg, Arc::clone(&clock));
            let trace = sim::run(&limiter, &clock, arrivals);
            let acc = compare(&trace, &oracle, limit, window_ns);
            AlgorithmRow {
                algorithm: a.name().to_owned(),
                blocked_pct: trace.denied() as f64 * 100.0 / arrivals.len().max(1) as f64,
                over_pct: acc.over_pct,
                under_pct: acc.under_pct,
                worst_window_ratio: acc.worst_window_ratio,
            }
        })
        .collect();

    let mut strategies = Vec::new();
    for &n in nodes {
        let placed = place(arrivals, n, None, 1);
        let ideal = metrics::ideal(cfg, &placed);
        for strategy in Strategy::defaults(cfg, n as u64) {
            progress(&format!("{} on {n} nodes", strategy.name()));
            let mut c = ClusterConfig::new(n);
            c.one_way_delay = Duration::from_micros(NETWORK.one_way_us);
            c.jitter = Duration::from_micros(NETWORK.jitter_us);
            c.timeout = Duration::from_millis(NETWORK.timeout_ms);
            c.seed = 1;
            let run = run_cluster(strategy, cfg, FailurePolicy::FailOpen, &c, &placed);
            let m = metrics::measure(&run, &ideal, cfg);
            strategies.push(StrategyRow {
                strategy: strategy.name().to_owned(),
                nodes: n,
                error_pct: m.over_pct + m.under_pct,
                over_pct: m.over_pct,
                under_pct: m.under_pct,
                calls_per_1k: m.calls_per_1k,
                p99_us: m.p99_us,
            });
        }
    }
    (limit, algorithms, strategies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use drl_core::{Cost, Nanos};

    fn burst(at_secs: u64, key: u32, n: usize) -> Vec<Arrival> {
        vec![
            Arrival {
                at: Nanos(at_secs * 1_000_000_000),
                key,
                cost: Cost::ONE,
            };
            n
        ]
    }

    #[test]
    fn limit_is_the_smallest_meeting_the_target() {
        // Client 0 sends 50 in one second; 99 other clients send 1 each: 149 requests.
        let mut a = burst(0, 0, 50);
        for k in 1..100 {
            a.extend(burst(1, k, 1));
        }
        a.sort_by_key(|x| x.at);
        // Blocking at most 1% of 149 allows one blocked request: limit 49.
        assert_eq!(choose_limit(&a, 1.0), 49);
        assert_eq!(choose_limit(&a, 0.0), 50);
    }

    #[test]
    fn replay_covers_every_algorithm_and_strategy() {
        let a = sim::poisson(1, Duration::from_secs(300), Duration::from_millis(50), 20);
        let (limit, algos, strats) = replay(&a, &[3], |_| {});
        assert!(limit >= 1);
        assert_eq!(algos.len(), 5);
        assert_eq!(strats.len(), 5);
        let sliding = algos
            .iter()
            .find(|r| r.algorithm == "sliding_window")
            .unwrap();
        assert_eq!(sliding.over_pct, 0.0);
        assert!(sliding.blocked_pct <= TARGET_BLOCKED_PCT);
    }
}
