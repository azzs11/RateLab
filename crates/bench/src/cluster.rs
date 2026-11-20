//! The multi-node run: what the `loadgen` binary records and how it is analysed.
//!
//! Every request's send time, key, node, route, status and decision source is recorded.
//! Accuracy is then computed offline against two references fed the same send times:
//!
//! - the exact [`ExactOracle`](crate::oracle::ExactOracle), which shows the cost of the
//!   algorithm itself (a token bucket allows bursts the oracle does not);
//! - one in-process token bucket, which is what a perfectly coordinated cluster would
//!   decide. The difference from it is the cost of distribution (network delay,
//!   reordering between nodes, Redis time versus send time).

use crate::machine::Machine;
use crate::oracle::ExactOracle;
use crate::stats::{Summary, percentile};
use ratelab::{Algorithm, Cost, Decision, ManualClock, Nanos, RateLimitConfig, Tokens};
use serde::{Deserialize, Serialize};
use sim::{Arrival, Step, Trace};
use std::sync::Arc;

/// Which route a request went to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Route {
    /// `GET /api/hello`, fails open.
    Public,
    /// `POST /api/orders`, fails closed.
    Orders,
}

impl Route {
    /// Route name as in the server's policy.
    pub fn name(self) -> &'static str {
        match self {
            Route::Public => "public",
            Route::Orders => "orders",
        }
    }
}

/// Who decided, from the `x-drl-source` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// Redis answered.
    Redis,
    /// Breaker open: the failure policy decided without calling Redis.
    BreakerOpen,
    /// Redis failed: the failure policy decided.
    RedisError,
    /// An in-process limiter (not expected in the cluster run).
    Local,
    /// A coordination strategy (the real-cluster strategy runs).
    Strategy,
    /// No response (connection error or timeout at the load generator).
    NoResponse,
}

impl Source {
    /// Parses the `x-drl-source` header value.
    pub fn from_header(value: Option<&str>) -> Self {
        match value {
            Some("redis") => Source::Redis,
            Some("breaker_open") => Source::BreakerOpen,
            Some("redis_error") => Source::RedisError,
            Some("local") => Source::Local,
            Some("strategy") => Source::Strategy,
            _ => Source::NoResponse,
        }
    }
}

/// One request as seen by the load generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    /// Send time since the start of the run, in nanoseconds.
    pub at_ns: u64,
    /// Client key.
    pub key: u32,
    /// Index of the node it was sent to.
    pub node: usize,
    /// Route.
    pub route: Route,
    /// HTTP status (0 if there was no response).
    pub status: u16,
    /// Time until the response, in microseconds.
    pub latency_us: u64,
    /// Who decided.
    pub source: Source,
}

impl Sample {
    /// Whether the request was let through.
    pub fn allowed(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Accuracy of one run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunAccuracy {
    /// Requests sent.
    pub requests: u64,
    /// Allowed but the oracle would deny (% of requests).
    pub over_vs_oracle_pct: f64,
    /// Denied but the oracle would allow (% of requests).
    pub under_vs_oracle_pct: f64,
    /// Allowed but one in-process token bucket would deny (% of requests).
    pub over_vs_single_pct: f64,
    /// Denied but one in-process token bucket would allow (% of requests).
    pub under_vs_single_pct: f64,
    /// Most units one key got in one window, divided by the limit.
    pub worst_window_ratio: f64,
}

fn arrivals(samples: &[Sample]) -> Vec<Arrival> {
    samples
        .iter()
        .map(|s| Arrival {
            at: Nanos(s.at_ns),
            key: s.key,
            cost: Cost::ONE,
        })
        .collect()
}

/// Reference decisions from one in-process token bucket on the same send times.
pub fn single_bucket(config: RateLimitConfig, samples: &[Sample]) -> Vec<bool> {
    let clock = Arc::new(ManualClock::new());
    let limiter = Algorithm::TokenBucket.build(config, Arc::clone(&clock));
    sim::run(&limiter, &clock, &arrivals(samples))
        .steps
        .iter()
        .map(|s| s.decision.allowed)
        .collect()
}

/// Compares one run (samples of one route, sorted by send time) with both references.
pub fn accuracy(config: RateLimitConfig, samples: &[Sample]) -> RunAccuracy {
    let oracle = ExactOracle::decide_all(config.limit(), config.window_nanos(), &arrivals(samples));
    let single = single_bucket(config, samples);
    let n = samples.len() as u64;
    let pct = |count: usize| {
        if n == 0 {
            0.0
        } else {
            count as f64 * 100.0 / n as f64
        }
    };
    let count = |reference: &[bool], over: bool| {
        samples
            .iter()
            .zip(reference.iter().copied())
            .filter(|&(s, r)| {
                if over {
                    s.allowed() && !r
                } else {
                    !s.allowed() && r
                }
            })
            .count()
    };
    let trace = Trace {
        steps: samples
            .iter()
            .zip(arrivals(samples))
            .map(|(s, arrival)| Step {
                arrival,
                decision: if s.allowed() {
                    Decision::allow(Tokens(0), std::time::Duration::ZERO)
                } else {
                    Decision::deny(Tokens(0), std::time::Duration::ZERO, None)
                },
            })
            .collect(),
    };
    RunAccuracy {
        requests: n,
        over_vs_oracle_pct: pct(count(&oracle, true)),
        under_vs_oracle_pct: pct(count(&oracle, false)),
        over_vs_single_pct: pct(count(&single, true)),
        under_vs_single_pct: pct(count(&single, false)),
        worst_window_ratio: trace.max_admitted_in_window(config.window()) as f64
            / config.limit() as f64,
    }
}

/// p50, p99 and p99.9 of the latencies, in milliseconds.
pub fn latency_ms(samples: &[Sample]) -> (f64, f64, f64) {
    let mut us: Vec<u64> = samples
        .iter()
        .filter(|s| s.source != Source::NoResponse)
        .map(|s| s.latency_us)
        .collect();
    us.sort_unstable();
    let ms = |p| percentile(&us, p) as f64 / 1_000.0;
    (ms(50.0), ms(99.0), ms(99.9))
}

/// One traffic pattern across the cluster, over every seed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioRow {
    /// `uniform` (requests spread evenly over nodes) or `skewed` (most go to node 0).
    pub scenario: String,
    /// Share of requests sent to node 0, in %.
    pub node0_share_pct: f64,
    /// Requests per run.
    pub requests: Summary,
    /// Requests per second actually sent.
    pub sent_per_sec: Summary,
    /// Over-admission against the exact oracle, %.
    pub over_vs_oracle_pct: Summary,
    /// Under-admission against the exact oracle, %.
    pub under_vs_oracle_pct: Summary,
    /// Over-admission against one in-process token bucket, %.
    pub over_vs_single_pct: Summary,
    /// Under-admission against one in-process token bucket, %.
    pub under_vs_single_pct: Summary,
    /// Worst units in one window / limit.
    pub worst_window_ratio: Summary,
    /// Redis calls (client round trips: EVALSHA, EVAL, SCRIPT LOAD) per 1,000 decisions,
    /// from Redis `INFO commandstats`.
    pub redis_calls_per_1k: Summary,
    /// Commands Redis executed per 1,000 decisions, including those inside the Lua
    /// script, from Redis `INFO stats`.
    pub redis_commands_per_1k: Summary,
    /// Redis network bytes (in + out) per decision.
    pub redis_bytes_per_decision: Summary,
    /// Client-observed latency p50, ms.
    pub p50_ms: Summary,
    /// Client-observed latency p99, ms.
    pub p99_ms: Summary,
    /// Client-observed latency p99.9, ms.
    pub p999_ms: Summary,
    /// Requests with no response, %.
    pub no_response_pct: Summary,
}

/// Closed-loop HTTP throughput across the nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThroughputRow {
    /// Requests in flight at once.
    pub concurrency: usize,
    /// Seconds per run.
    pub secs: u64,
    /// Completed requests per second.
    pub requests_per_sec: Summary,
    /// Latency p50, ms.
    pub p50_ms: Summary,
    /// Latency p99, ms.
    pub p99_ms: Summary,
}

/// One phase of the outage run for one route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResilienceRow {
    /// `before`, `during` or `after` the outage.
    pub phase: String,
    /// Route policy name.
    pub route: String,
    /// `open` or `closed`.
    pub failure_policy: String,
    /// Requests in this phase.
    pub requests: u64,
    /// Requests let through, %.
    pub allowed_pct: f64,
    /// Allowed but the oracle would deny, % of requests.
    pub over_vs_oracle_pct: f64,
    /// Decided by Redis, %.
    pub redis_pct: f64,
    /// Decided by the failure policy with the breaker open, %.
    pub breaker_open_pct: f64,
    /// Decided by the failure policy after a Redis error or timeout, %.
    pub redis_error_pct: f64,
    /// No response, %.
    pub no_response_pct: f64,
    /// Latency p99 in this phase, ms.
    pub p99_ms: f64,
}

/// Splits an outage run into phases and summarises each route.
pub fn resilience(
    config: RateLimitConfig,
    samples: &[Sample],
    outage_start_ns: u64,
    outage_end_ns: u64,
) -> Vec<ResilienceRow> {
    let mut rows = Vec::new();
    for (route, policy) in [(Route::Public, "open"), (Route::Orders, "closed")] {
        let mine: Vec<Sample> = samples
            .iter()
            .copied()
            .filter(|s| s.route == route)
            .collect();
        let oracle =
            ExactOracle::decide_all(config.limit(), config.window_nanos(), &arrivals(&mine));
        for (phase, lo, hi) in [
            ("before", 0, outage_start_ns),
            ("during", outage_start_ns, outage_end_ns),
            ("after", outage_end_ns, u64::MAX),
        ] {
            let in_phase: Vec<(&Sample, bool)> = mine
                .iter()
                .zip(oracle.iter().copied())
                .filter(|(s, _)| (lo..hi).contains(&s.at_ns))
                .collect();
            let n = in_phase.len();
            let pct = |f: &dyn Fn(&Sample, bool) -> bool| {
                if n == 0 {
                    0.0
                } else {
                    in_phase.iter().filter(|(s, o)| f(s, *o)).count() as f64 * 100.0 / n as f64
                }
            };
            let phase_samples: Vec<Sample> = in_phase.iter().map(|(s, _)| **s).collect();
            rows.push(ResilienceRow {
                phase: phase.to_owned(),
                route: route.name().to_owned(),
                failure_policy: policy.to_owned(),
                requests: n as u64,
                allowed_pct: pct(&|s, _| s.allowed()),
                over_vs_oracle_pct: pct(&|s, o| s.allowed() && !o),
                redis_pct: pct(&|s, _| s.source == Source::Redis),
                breaker_open_pct: pct(&|s, _| s.source == Source::BreakerOpen),
                redis_error_pct: pct(&|s, _| s.source == Source::RedisError),
                no_response_pct: pct(&|s, _| s.source == Source::NoResponse),
                p99_ms: latency_ms(&phase_samples).1,
            });
        }
    }
    rows
}

/// The servers' failure settings, from the same environment as the nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakerSettings {
    /// Consecutive failures that open a node's breaker.
    pub failures: u32,
    /// How long it stays open, ms.
    pub open_ms: u64,
    /// Redis reply timeout, ms.
    pub redis_timeout_ms: u64,
}

/// `results/cluster.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterResults {
    /// File format version.
    pub schema: u32,
    /// The machine as seen from inside the load generator container.
    pub machine: Machine,
    /// The host, as described by `bench machine` outside Docker (if given).
    pub host: Option<String>,
    /// Server nodes.
    pub nodes: usize,
    /// Per-key limit, read from the servers' `RateLimit-Policy` header.
    pub limit: u64,
    /// Window in seconds, read from the same header.
    pub window_secs: u64,
    /// `full` or `quick`.
    pub size: String,
    /// Seconds per scenario run.
    pub run_secs: u64,
    /// Seeds per scenario.
    pub seeds: u64,
    /// Client keys.
    pub keys: u32,
    /// Offered load per key, as a multiple of the limit.
    pub load_factor: f64,
    /// Accuracy, cost and latency per scenario.
    pub scenarios: Vec<ScenarioRow>,
    /// Closed-loop throughput.
    pub throughput: Option<ThroughputRow>,
    /// The nodes' breaker settings, if the load generator was given them.
    pub breaker: Option<BreakerSettings>,
    /// When the outage started and ended, seconds into the resilience run.
    pub outage_secs: (f64, f64),
    /// The outage run, per phase and route.
    pub resilience: Vec<ResilienceRow>,
    /// After the outage ended, time until every node answered from Redis again, ms.
    pub recovery_ms: Option<f64>,
}

/// After `end_ns`, how long until every node had answered a request from Redis
/// again (the slowest node's first Redis decision), in milliseconds. `None` if some
/// node never recovered within the run.
pub fn recovery_ms(samples: &[Sample], nodes: usize, end_ns: u64) -> Option<f64> {
    (0..nodes)
        .map(|node| {
            samples
                .iter()
                .filter(|s| s.node == node && s.at_ns >= end_ns && s.source == Source::Redis)
                .map(|s| s.at_ns - end_ns)
                .min()
        })
        .try_fold(0u64, |worst, first| first.map(|f| worst.max(f)))
        .map(|ns| ns as f64 / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn sample(at_ms: u64, status: u16, route: Route, source: Source) -> Sample {
        Sample {
            at_ns: at_ms * 1_000_000,
            key: 0,
            node: 0,
            route,
            status,
            latency_us: 1_000,
            source,
        }
    }

    #[test]
    fn a_perfect_cluster_matches_the_single_bucket() {
        let config = RateLimitConfig::per_second(2).unwrap();
        // A token bucket of 2/s: two allowed, third denied, one refilled after 500 ms.
        let s = [
            sample(0, 200, Route::Public, Source::Redis),
            sample(1, 200, Route::Public, Source::Redis),
            sample(2, 429, Route::Public, Source::Redis),
            sample(600, 200, Route::Public, Source::Redis),
        ];
        let acc = accuracy(config, &s);
        assert_eq!(acc.over_vs_single_pct, 0.0);
        assert_eq!(acc.under_vs_single_pct, 0.0);
        // The oracle denies the one at 600 ms: 3 in the same 1 s span.
        assert_eq!(acc.over_vs_oracle_pct, 25.0);
        assert_eq!(acc.worst_window_ratio, 1.5);
    }

    #[test]
    fn over_admission_is_counted_against_both_references() {
        let config = RateLimitConfig::per_second(1).unwrap();
        let s = [
            sample(0, 200, Route::Public, Source::Redis),
            sample(1, 200, Route::Public, Source::Redis),
        ];
        let acc = accuracy(config, &s);
        assert_eq!(acc.over_vs_single_pct, 50.0);
        assert_eq!(acc.over_vs_oracle_pct, 50.0);
        assert_eq!(acc.worst_window_ratio, 2.0);
    }

    #[test]
    fn resilience_splits_by_phase_and_route() {
        let config = RateLimitConfig::new(100, Duration::from_secs(1)).unwrap();
        let s = [
            sample(100, 200, Route::Public, Source::Redis),
            sample(1_500, 200, Route::Public, Source::BreakerOpen),
            sample(1_600, 429, Route::Orders, Source::BreakerOpen),
            sample(2_500, 201, Route::Orders, Source::Redis),
        ];
        let rows = resilience(config, &s, 1_000_000_000, 2_000_000_000);
        let find = |phase: &str, route: &str| {
            rows.iter()
                .find(|r| r.phase == phase && r.route == route)
                .unwrap()
                .clone()
        };
        assert_eq!(find("during", "public").allowed_pct, 100.0);
        assert_eq!(find("during", "public").breaker_open_pct, 100.0);
        assert_eq!(find("during", "orders").allowed_pct, 0.0);
        assert_eq!(find("after", "orders").redis_pct, 100.0);
        assert_eq!(find("before", "orders").requests, 0);
    }

    #[test]
    fn recovery_waits_for_the_slowest_node() {
        let mut a = sample(2_100, 200, Route::Public, Source::Redis);
        let mut b = sample(2_400, 200, Route::Public, Source::Redis);
        b.node = 1;
        let early = sample(1_900, 200, Route::Public, Source::Redis);
        assert_eq!(recovery_ms(&[early, a, b], 2, 2_000_000_000), Some(400.0));
        a.node = 1;
        assert_eq!(recovery_ms(&[a, b], 2, 2_000_000_000), None);
    }

    #[test]
    fn source_header_parsing() {
        assert_eq!(Source::from_header(Some("redis")), Source::Redis);
        assert_eq!(
            Source::from_header(Some("breaker_open")),
            Source::BreakerOpen
        );
        assert_eq!(Source::from_header(None), Source::NoResponse);
    }
}
