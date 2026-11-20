//! Coordination strategies compared in the deterministic multi-node simulator.
//!
//! Every run is compared with one perfectly coordinated token bucket fed the same
//! arrival times (the **ideal**): what a single exact limiter would have decided.
//!
//! **Admission error** counts units, per key and per window (1 s): in each window,
//! units admitted above what the ideal admitted are **over-admission**, units missing
//! below it are **under-admission**, both as % of everything the ideal admitted. Which
//! particular requests get through under overload does not matter, only how many. The
//! share of individual decisions that differ from the ideal is kept as `disagree_pct`.
//!
//! The network, delay and outage are simulated, so these runs measure the protocols,
//! not a particular machine; every run is reproducible from its seed.

use crate::local::{ACCURACY_LIMIT, Scenario};
use crate::stats::Summary;
use drl_core::strategies::Strategy;
use drl_core::{FailurePolicy, Nanos, RateLimitConfig};
use serde::{Deserialize, Serialize};
use sim::cluster::{ClusterArrival, ClusterConfig, ClusterRun, place, run_cluster};
use sim::metrics::{self, Measured, WindowCounts};
use std::ops::Range;
use std::time::Duration;

/// How much work the strategy suite does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategySize {
    /// `full` or `quick`.
    pub name: String,
    /// Seconds per run.
    pub secs: u64,
    /// Seeds per configuration.
    pub seeds: u64,
    /// Cluster sizes.
    pub nodes: Vec<usize>,
}

impl StrategySize {
    /// The size used for RESULTS.md.
    pub fn full() -> Self {
        Self {
            name: "full".to_owned(),
            secs: 20,
            seeds: 5,
            nodes: vec![1, 3, 8],
        }
    }

    /// A fast size for CI; the numbers are not meaningful.
    pub fn quick() -> Self {
        Self {
            name: "quick".to_owned(),
            secs: 2,
            seeds: 1,
            nodes: vec![1, 3],
        }
    }
}

/// The simulated network every run uses.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Network {
    /// Fixed one-way delay node ↔ store, µs.
    pub one_way_us: u64,
    /// Extra uniform one-way jitter, µs.
    pub jitter_us: u64,
    /// Node timeout for a store reply, ms.
    pub timeout_ms: u64,
}

/// The network model: 250 µs one way plus up to 250 µs jitter, 100 ms timeout.
pub const NETWORK: Network = Network {
    one_way_us: 250,
    jitter_us: 250,
    timeout_ms: 100,
};

fn cluster(nodes: usize, seed: u64) -> ClusterConfig {
    let mut c = ClusterConfig::new(nodes);
    c.one_way_delay = Duration::from_micros(NETWORK.one_way_us);
    c.jitter = Duration::from_micros(NETWORK.jitter_us);
    c.timeout = Duration::from_millis(NETWORK.timeout_ms);
    c.seed = seed;
    c
}

fn config() -> RateLimitConfig {
    RateLimitConfig::per_second(ACCURACY_LIMIT).expect("constant, valid config")
}

/// Name of the skewed traffic pattern: steady Poisson with 80% of requests on node 0.
pub const SKEWED: &str = "skewed";

/// Every traffic pattern the suite runs, in report order.
pub fn traffic_names() -> Vec<&'static str> {
    let mut names: Vec<&str> = Scenario::ALL.iter().map(|s| s.name()).collect();
    names.insert(1, SKEWED);
    names
}

fn traffic(name: &str, seed: u64, secs: u64, nodes: usize) -> Vec<ClusterArrival> {
    if name == SKEWED {
        let a = Scenario::SteadyPoisson.arrivals(seed, secs);
        return place(&a, nodes, Some(80), seed);
    }
    let scenario = Scenario::ALL
        .into_iter()
        .find(|s| s.name() == name)
        .unwrap_or(Scenario::SteadyPoisson);
    place(&scenario.arrivals(seed, secs), nodes, None, seed)
}

/// Decisions of the ideal (one exact token bucket) on the same arrival times.
pub fn ideal(a: &[ClusterArrival]) -> Vec<bool> {
    metrics::ideal(config(), a)
}

fn measure(run: &ClusterRun, ideal: &[bool]) -> Measured {
    metrics::measure(run, ideal, config())
}

fn window_counts(run: &ClusterRun, ideal: &[bool], range: Range<u64>, bin_ns: u64) -> WindowCounts {
    metrics::window_counts(run, ideal, range, bin_ns)
}

/// One strategy on one traffic pattern and cluster size, over every seed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyRow {
    /// Strategy name.
    pub strategy: String,
    /// Traffic pattern.
    pub scenario: String,
    /// Nodes.
    pub nodes: usize,
    /// Requests per run.
    pub requests: Summary,
    /// Over-admission, % of the ideal's admitted units.
    pub over_pct: Summary,
    /// Under-admission, % of the ideal's admitted units.
    pub under_pct: Summary,
    /// Admission error: over + under, %.
    pub error_pct: Summary,
    /// Individual decisions that differ from the ideal, % of requests.
    pub disagree_pct: Summary,
    /// Admitted vs ideal admitted, %.
    pub admitted_vs_ideal_pct: Summary,
    /// Worst units in one window / limit.
    pub worst_window_ratio: Summary,
    /// Store calls per 1,000 decisions.
    pub calls_per_1k: Summary,
    /// Decision latency p50, µs.
    pub p50_us: Summary,
    /// p99, µs.
    pub p99_us: Summary,
    /// p99.9, µs.
    pub p999_us: Summary,
}

fn summarise(runs: &[Measured], f: fn(&Measured) -> f64) -> Summary {
    Summary::of(&runs.iter().map(f).collect::<Vec<_>>())
}

/// Every strategy × traffic pattern × cluster size.
pub fn matrix(size: &StrategySize, mut progress: impl FnMut(&str)) -> Vec<StrategyRow> {
    let mut rows = Vec::new();
    for &nodes in &size.nodes {
        for name in traffic_names() {
            progress(&format!("strategies {name} nodes={nodes}"));
            let inputs: Vec<_> = (1..=size.seeds)
                .map(|seed| {
                    let a = traffic(name, seed, size.secs, nodes);
                    let i = ideal(&a);
                    (seed, a, i)
                })
                .collect();
            for strategy in Strategy::defaults(config(), nodes as u64) {
                let runs: Vec<Measured> = std::thread::scope(|s| {
                    let handles: Vec<_> = inputs
                        .iter()
                        .map(|(seed, a, i)| {
                            s.spawn(move || {
                                let r = run_cluster(
                                    strategy,
                                    config(),
                                    FailurePolicy::FailOpen,
                                    &cluster(nodes, *seed),
                                    a,
                                );
                                measure(&r, i)
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| h.join().expect("simulation thread"))
                        .collect()
                });
                rows.push(StrategyRow {
                    strategy: strategy.name().to_owned(),
                    scenario: name.to_owned(),
                    nodes,
                    requests: summarise(&runs, |m| m.requests),
                    over_pct: summarise(&runs, |m| m.over_pct),
                    under_pct: summarise(&runs, |m| m.under_pct),
                    error_pct: summarise(&runs, |m| m.over_pct + m.under_pct),
                    disagree_pct: summarise(&runs, |m| m.disagree_pct),
                    admitted_vs_ideal_pct: summarise(&runs, |m| m.admitted_vs_ideal_pct),
                    worst_window_ratio: summarise(&runs, |m| m.worst_window_ratio),
                    calls_per_1k: summarise(&runs, |m| m.calls_per_1k),
                    p50_us: summarise(&runs, |m| m.p50_us),
                    p99_us: summarise(&runs, |m| m.p99_us),
                    p999_us: summarise(&runs, |m| m.p999_us),
                });
            }
        }
    }
    rows
}

/// Clock skew between nodes, per strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkewRow {
    /// Strategy name.
    pub strategy: String,
    /// Each node's offset, ms.
    pub skew_ms: Vec<i64>,
    /// Admission error with no skew, %.
    pub error_no_skew_pct: Summary,
    /// Admission error with the skew, %.
    pub error_skewed_pct: Summary,
    /// Over-admission with the skew, %.
    pub over_skewed_pct: Summary,
}

/// Skew in ms for the three nodes of the skew experiment.
pub const SKEW_MS: [i64; 3] = [-50, 0, 50];

/// Steady traffic on 3 nodes with and without ±50 ms skew. Includes central exact with
/// node-supplied time, the way an application-clock script would decide.
pub fn skew(size: &StrategySize) -> Vec<SkewRow> {
    let mut strategies = Strategy::defaults(config(), 3).to_vec();
    strategies.insert(1, Strategy::CentralExact { node_clock: true });
    let inputs: Vec<_> = (1..=size.seeds)
        .map(|seed| {
            let a = traffic(Scenario::SteadyPoisson.name(), seed, size.secs, 3);
            let i = ideal(&a);
            (seed, a, i)
        })
        .collect();
    strategies
        .into_iter()
        .map(|strategy| {
            let mut plain = Vec::new();
            let mut skewed = Vec::new();
            for (seed, a, i) in &inputs {
                let mut c = cluster(3, *seed);
                plain.push(measure(
                    &run_cluster(strategy, config(), FailurePolicy::FailOpen, &c, a),
                    i,
                ));
                c.skew_ns = SKEW_MS.iter().map(|ms| ms * 1_000_000).collect();
                skewed.push(measure(
                    &run_cluster(strategy, config(), FailurePolicy::FailOpen, &c, a),
                    i,
                ));
            }
            SkewRow {
                strategy: strategy.name().to_owned(),
                skew_ms: SKEW_MS.to_vec(),
                error_no_skew_pct: summarise(&plain, |m| m.over_pct + m.under_pct),
                error_skewed_pct: summarise(&skewed, |m| m.over_pct + m.under_pct),
                over_skewed_pct: summarise(&skewed, |m| m.over_pct),
            }
        })
        .collect()
}

/// A store outage, per strategy and failure policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutageRow {
    /// Strategy name.
    pub strategy: String,
    /// `open` or `closed`.
    pub policy: String,
    /// During the outage: requests allowed, %.
    pub during_allowed_pct: Summary,
    /// During the outage: over-admission, % of the ideal's admitted units.
    pub during_over_pct: Summary,
    /// During the outage: under-admission, % of the ideal's admitted units.
    pub during_under_pct: Summary,
    /// Whole run: admission error, %.
    pub overall_error_pct: Summary,
}

/// The outage window as fractions of the run: from 40% to 60%.
pub const OUTAGE: (f64, f64) = (0.4, 0.6);

/// Steady traffic on 3 nodes; the store is unreachable for the middle fifth of the run.
pub fn outage(size: &StrategySize) -> Vec<OutageRow> {
    let span = size.secs as f64 * 1e9;
    let (start, end) = ((span * OUTAGE.0) as u64, (span * OUTAGE.1) as u64);
    let inputs: Vec<_> = (1..=size.seeds)
        .map(|seed| {
            let a = traffic(Scenario::SteadyPoisson.name(), seed, size.secs, 3);
            let i = ideal(&a);
            (seed, a, i)
        })
        .collect();
    let mut rows = Vec::new();
    for strategy in Strategy::defaults(config(), 3) {
        for policy in [FailurePolicy::FailOpen, FailurePolicy::FailClosed] {
            let mut cols: [Vec<f64>; 4] = Default::default();
            for (seed, a, i) in &inputs {
                let mut c = cluster(3, *seed);
                c.outage = Some((Nanos(start), Nanos(end)));
                let r = run_cluster(strategy, config(), policy, &c, a);
                let during: Vec<bool> = r
                    .outcomes
                    .iter()
                    .filter(|o| (start..end).contains(&o.arrival.at.0))
                    .map(|o| o.decision.allowed)
                    .collect();
                let counts = window_counts(&r, i, start..end, config().window_nanos());
                let m = measure(&r, i);
                cols[0].push(
                    during.iter().filter(|&&a| a).count() as f64 * 100.0
                        / during.len().max(1) as f64,
                );
                cols[1].push(counts.over_pct());
                cols[2].push(counts.under_pct());
                cols[3].push(m.over_pct + m.under_pct);
            }
            rows.push(OutageRow {
                strategy: strategy.name().to_owned(),
                policy: policy.name().to_owned(),
                during_allowed_pct: Summary::of(&cols[0]),
                during_over_pct: Summary::of(&cols[1]),
                during_under_pct: Summary::of(&cols[2]),
                overall_error_pct: Summary::of(&cols[3]),
            });
        }
    }
    rows
}

/// How each strategy copes when traffic moves from one node to another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceRow {
    /// Strategy name.
    pub strategy: String,
    /// Under-admission in the first second after the shift, % of the ideal's admitted units.
    pub under_first_second_pct: Summary,
    /// Time after the shift until three 100 ms bins in a row admit at least 90% of what
    /// the ideal admits, ms. Runs that never get there are counted in `never`.
    pub rebalance_ms: Summary,
    /// Seeds that did not rebalance before the run ended.
    pub never: usize,
}

const BIN_NS: u64 = 100_000_000;

/// When traffic moved, three bins in a row reaching 90% of the ideal.
fn rebalance_ms(run: &ClusterRun, ideal: &[bool], shift: u64, end: u64) -> Option<f64> {
    let bins = ((end - shift) / BIN_NS) as usize;
    let mut got = vec![0u64; bins + 1];
    let mut want = vec![0u64; bins + 1];
    for (o, &ok) in run.outcomes.iter().zip(ideal) {
        let t = o.arrival.at.0;
        if t < shift {
            continue;
        }
        let b = ((t - shift) / BIN_NS) as usize;
        if b < got.len() {
            got[b] += u64::from(o.decision.allowed);
            want[b] += u64::from(ok);
        }
    }
    let good = |b: usize| want[b] == 0 || got[b] * 10 >= want[b] * 9;
    (0..bins.saturating_sub(2))
        .find(|&b| good(b) && good(b + 1) && good(b + 2))
        .map(|b| (b as u64 * BIN_NS) as f64 / 1e6)
}

/// Steady traffic on 3 nodes: all of it on node 0 for the first half, then all on node 1.
pub fn convergence(size: &StrategySize) -> Vec<ConvergenceRow> {
    let shift = size.secs * 1_000_000_000 / 2;
    let end = size.secs * 1_000_000_000;
    let inputs: Vec<_> = (1..=size.seeds)
        .map(|seed| {
            let a: Vec<ClusterArrival> = Scenario::SteadyPoisson
                .arrivals(seed, size.secs)
                .into_iter()
                .map(|arrival| ClusterArrival {
                    arrival,
                    node: usize::from(arrival.at.0 >= shift),
                })
                .collect();
            let i = ideal(&a);
            (seed, a, i)
        })
        .collect();
    Strategy::defaults(config(), 3)
        .into_iter()
        .map(|strategy| {
            let mut under = Vec::new();
            let mut times = Vec::new();
            let mut never = 0;
            for (seed, a, i) in &inputs {
                let r = run_cluster(
                    strategy,
                    config(),
                    FailurePolicy::FailOpen,
                    &cluster(3, *seed),
                    a,
                );
                let counts = window_counts(&r, i, shift..shift + 1_000_000_000, 1_000_000_000);
                under.push(counts.under_pct());
                match rebalance_ms(&r, i, shift, end) {
                    Some(ms) => times.push(ms),
                    None => never += 1,
                }
            }
            ConvergenceRow {
                strategy: strategy.name().to_owned(),
                under_first_second_pct: Summary::of(&under),
                rebalance_ms: Summary::of(&times),
                never,
            }
        })
        .collect()
}

/// Another strategy that beat the target on both axes in one configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Dominance {
    /// Traffic pattern.
    pub scenario: String,
    /// Nodes.
    pub nodes: usize,
    /// The strategy that did better.
    pub by: String,
    /// Its admission error, %.
    pub by_error: f64,
    /// The target's admission error, %.
    pub error: f64,
    /// Its store calls per 1,000 decisions.
    pub by_calls: f64,
    /// The target's store calls per 1,000 decisions.
    pub calls: f64,
}

/// Strategies that beat `target` on both axes (fewer or equal calls and lower error, or
/// fewer calls and equal error) for each scenario and cluster size.
pub fn dominated_by(rows: &[StrategyRow], target: &str) -> Vec<Dominance> {
    let mut out = Vec::new();
    for t in rows.iter().filter(|r| r.strategy == target) {
        for o in rows
            .iter()
            .filter(|r| r.strategy != target && r.scenario == t.scenario && r.nodes == t.nodes)
        {
            let (oc, oe) = (o.calls_per_1k.mean, o.error_pct.mean);
            let (tc, te) = (t.calls_per_1k.mean, t.error_pct.mean);
            if (oc <= tc && oe < te) || (oc < tc && oe <= te) {
                out.push(Dominance {
                    scenario: t.scenario.clone(),
                    nodes: t.nodes,
                    by: o.strategy.clone(),
                    by_error: oe,
                    error: te,
                    by_calls: oc,
                    calls: tc,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_suite_produces_every_row() {
        let size = StrategySize::quick();
        let rows = matrix(&size, |_| {});
        assert_eq!(rows.len(), size.nodes.len() * traffic_names().len() * 5);
        let local_split_1 = rows
            .iter()
            .find(|r| r.strategy == "local_split" && r.nodes == 1 && r.scenario == "steady_poisson")
            .unwrap();
        assert_eq!(local_split_1.error_pct.mean, 0.0);
        assert_eq!(local_split_1.calls_per_1k.mean, 0.0);

        let skew = skew(&size);
        let node_clock = skew
            .iter()
            .find(|r| r.strategy == "central_exact_node_clock")
            .unwrap();
        let store_clock = skew.iter().find(|r| r.strategy == "central_exact").unwrap();
        assert!(node_clock.error_skewed_pct.mean > store_clock.error_skewed_pct.mean);

        let outage = outage(&size);
        assert_eq!(outage.len(), 10);
        let conv = convergence(&size);
        assert_eq!(conv.len(), 5);
    }

    #[test]
    fn rebalance_needs_three_good_bins() {
        let ms = 1_000_000;
        // A run where nothing is admitted in the first 300 ms after the shift.
        let a: Vec<ClusterArrival> = (0..100)
            .map(|i| ClusterArrival {
                arrival: sim::Arrival {
                    at: Nanos(i * 10 * ms),
                    key: 0,
                    cost: drl_core::Cost::ONE,
                },
                node: 0,
            })
            .collect();
        let run = ClusterRun {
            outcomes: a
                .iter()
                .map(|c| sim::cluster::Outcome {
                    arrival: c.arrival,
                    node: 0,
                    decided_at: c.arrival.at,
                    decision: if c.arrival.at.0 < 300 * ms {
                        drl_core::Decision::deny(drl_core::Tokens(0), Duration::ZERO, None)
                    } else {
                        drl_core::Decision::allow(drl_core::Tokens(0), Duration::ZERO)
                    },
                })
                .collect(),
            undecided: 0,
            messages_sent: 0,
            store_ops: 0,
            timeouts: 0,
        };
        let ideal = vec![true; a.len()];
        assert_eq!(rebalance_ms(&run, &ideal, 0, 1_000 * ms), Some(300.0));
    }

    #[test]
    fn domination_needs_both_axes() {
        let row = |s: &str, calls: f64, err: f64| StrategyRow {
            strategy: s.to_owned(),
            scenario: "x".to_owned(),
            nodes: 3,
            requests: Summary::of(&[1.0]),
            over_pct: Summary::of(&[0.0]),
            under_pct: Summary::of(&[0.0]),
            error_pct: Summary::of(&[err]),
            disagree_pct: Summary::of(&[0.0]),
            admitted_vs_ideal_pct: Summary::of(&[0.0]),
            worst_window_ratio: Summary::of(&[1.0]),
            calls_per_1k: Summary::of(&[calls]),
            p50_us: Summary::of(&[0.0]),
            p99_us: Summary::of(&[0.0]),
            p999_us: Summary::of(&[0.0]),
        };
        let rows = [
            row("adaptive_leasing", 100.0, 5.0),
            row("a", 50.0, 4.0),
            row("b", 50.0, 6.0),
            row("c", 200.0, 1.0),
        ];
        let d = dominated_by(&rows, "adaptive_leasing");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].by, "a");
    }
}
