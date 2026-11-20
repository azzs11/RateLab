//! The coordination strategies on the real cluster, compared with the simulator.
//!
//! The load generator records every request of a real run (send time, client, node,
//! outcome, latency). That run is measured exactly the way the simulator's runs are
//! ([`sim::metrics::measure`]), and the same recorded requests (same times, same
//! nodes) are replayed through the simulator. If the two agree, the simulator's results
//! for configurations a laptop cannot run for real deserve more trust; where they differ,
//! the simulator's model is wrong there.

use crate::cluster::Sample;
use crate::stats::Summary;
use crate::strategies::NETWORK;
use ratelab::strategies::Strategy;
use ratelab::{Cost, Decision, FailurePolicy, Nanos, RateLimitConfig, Tokens};
use serde::{Deserialize, Serialize};
use sim::Arrival;
use sim::cluster::{ClusterArrival, ClusterConfig, ClusterRun, Outcome, run_cluster};
use sim::metrics::{self, Measured};
use std::time::Duration;

/// The recorded requests as cluster arrivals (sorted by send time).
pub fn arrivals(samples: &[Sample]) -> Vec<ClusterArrival> {
    samples
        .iter()
        .map(|s| ClusterArrival {
            arrival: Arrival {
                at: Nanos(s.at_ns),
                key: s.key,
                cost: Cost::ONE,
            },
            node: s.node,
        })
        .collect()
}

/// A real run as a [`ClusterRun`], so it is measured with the simulator's own code.
pub fn as_run(samples: &[Sample], store_calls: u64) -> ClusterRun {
    ClusterRun {
        outcomes: samples
            .iter()
            .map(|s| Outcome {
                arrival: Arrival {
                    at: Nanos(s.at_ns),
                    key: s.key,
                    cost: Cost::ONE,
                },
                node: s.node,
                decided_at: Nanos(s.at_ns + s.latency_us * 1_000),
                decision: if s.allowed() {
                    Decision::allow(Tokens(0), Duration::ZERO)
                } else {
                    Decision::deny(Tokens(0), Duration::ZERO, None)
                },
            })
            .collect(),
        undecided: 0,
        messages_sent: store_calls,
        store_ops: store_calls,
        timeouts: 0,
        messages: Vec::new(),
        samples: Vec::new(),
    }
}

/// Measures a real run and its simulated twin on the same requests.
pub fn compare(
    strategy: Strategy,
    config: RateLimitConfig,
    nodes: usize,
    samples: &[Sample],
    store_calls: u64,
) -> (Measured, Measured) {
    let placed = arrivals(samples);
    let ideal = metrics::ideal(config, &placed);
    let real = metrics::measure(&as_run(samples, store_calls), &ideal, config);
    let mut c = ClusterConfig::new(nodes);
    c.one_way_delay = Duration::from_micros(NETWORK.one_way_us);
    c.jitter = Duration::from_micros(NETWORK.jitter_us);
    c.timeout = Duration::from_millis(NETWORK.timeout_ms);
    c.seed = 1;
    let simulated = run_cluster(strategy, config, FailurePolicy::FailOpen, &c, &placed);
    (real, metrics::measure(&simulated, &ideal, config))
}

/// One strategy and traffic pattern, real against simulated, over every seed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    /// Strategy name.
    pub strategy: String,
    /// `uniform` or `skewed`.
    pub scenario: String,
    /// Share of requests on node 0, %.
    pub node0_share_pct: f64,
    /// Real: admission error, %.
    pub real_error_pct: Summary,
    /// Simulated: admission error, %.
    pub sim_error_pct: Summary,
    /// Real: store (Redis) calls per 1,000 decisions.
    pub real_calls_per_1k: Summary,
    /// Simulated: store calls per 1,000 decisions.
    pub sim_calls_per_1k: Summary,
    /// Real: client-observed p99 latency, µs (includes HTTP and Docker networking).
    pub real_p99_us: Summary,
    /// Simulated: decision wait p99, µs (the decision only).
    pub sim_p99_us: Summary,
}

/// `results/real_strategies.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealStrategyResults {
    /// File format version.
    pub schema: u32,
    /// The machine as seen from inside the load generator container.
    pub machine: crate::machine::Machine,
    /// The host, as described by `bench machine` outside Docker (if given).
    pub host: Option<String>,
    /// Nodes.
    pub nodes: usize,
    /// Per-key limit.
    pub limit: u64,
    /// Window, seconds.
    pub window_secs: u64,
    /// Client keys.
    pub keys: u32,
    /// Offered load per key, × the limit.
    pub load_factor: f64,
    /// Seconds per run.
    pub run_secs: u64,
    /// Seeds per configuration.
    pub seeds: u64,
    /// One row per strategy and scenario.
    pub rows: Vec<Row>,
}

/// Collects one configuration's seeds into a row.
pub fn row(strategy: &str, scenario: &str, node0: f64, runs: &[(Measured, Measured)]) -> Row {
    let s =
        |f: fn(&(Measured, Measured)) -> f64| Summary::of(&runs.iter().map(f).collect::<Vec<_>>());
    Row {
        strategy: strategy.to_owned(),
        scenario: scenario.to_owned(),
        node0_share_pct: node0,
        real_error_pct: s(|r| r.0.over_pct + r.0.under_pct),
        sim_error_pct: s(|r| r.1.over_pct + r.1.under_pct),
        real_calls_per_1k: s(|r| r.0.calls_per_1k),
        sim_calls_per_1k: s(|r| r.1.calls_per_1k),
        real_p99_us: s(|r| r.0.p99_us),
        sim_p99_us: s(|r| r.1.p99_us),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{Route, Source};

    fn sample(at_ms: u64, node: usize, status: u16) -> Sample {
        Sample {
            at_ns: at_ms * 1_000_000,
            key: 0,
            node,
            route: Route::Public,
            status,
            latency_us: 800,
            source: Source::Local,
        }
    }

    #[test]
    fn a_real_run_is_measured_like_a_simulated_one() {
        let config = RateLimitConfig::per_second(2).unwrap();
        // Ideal bucket of 2/s: allow, allow, deny.
        let samples = [sample(0, 0, 200), sample(1, 1, 200), sample(2, 2, 429)];
        let (real, sim) = compare(
            Strategy::CentralExact { node_clock: false },
            config,
            3,
            &samples,
            3,
        );
        assert_eq!(real.over_pct + real.under_pct, 0.0);
        assert_eq!(real.calls_per_1k, 1_000.0);
        assert_eq!(real.p99_us, 800.0);
        // The simulated twin of central exact also asks the store for every request.
        assert_eq!(sim.calls_per_1k, 1_000.0);
    }

    #[test]
    fn rows_summarise_both_sides() {
        let config = RateLimitConfig::per_second(2).unwrap();
        let samples = [sample(0, 0, 200), sample(1, 1, 200), sample(2, 2, 200)];
        let runs = [compare(Strategy::LocalSplit, config, 3, &samples, 0)];
        let r = row("local_split", "uniform", 33.3, &runs);
        assert!(r.real_error_pct.mean > 0.0); // the third request is over the limit
        assert_eq!(r.real_calls_per_1k.mean, 0.0);
    }
}
