//! The cluster simulator in the browser: the same `sim::cluster` and `sim::metrics` code
//! that produced RESULTS.md.

use crate::traffic::{Traffic, config, ms_to_nanos};
use ratelab::strategies::{StoreOp, StoreReply, Strategy};
use ratelab::{FailurePolicy, Nanos};
use sim::cluster::{ClusterConfig, place, run_cluster};
use sim::metrics;
use std::time::Duration;
use wasm_bindgen::prelude::*;

/// Names of the five strategies, in report order.
pub const STRATEGIES: [&str; 5] = [
    "central_exact",
    "local_split",
    "async_sync",
    "fixed_leasing",
    "adaptive_leasing",
];

/// Settings for one cluster run.
#[wasm_bindgen(getter_with_clone)]
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterOptions {
    /// One of [`strategies`].
    pub strategy: String,
    /// Nodes (1 to 16).
    pub nodes: u32,
    /// Units per window per key, across the whole cluster.
    pub limit: u32,
    /// Window, ms.
    pub window_ms: f64,
    /// One-way delay node ↔ store, µs.
    pub delay_us: f64,
    /// Extra uniform one-way jitter, µs.
    pub jitter_us: f64,
    /// Clock offset between neighbouring nodes, ms (node i is offset by i × skew).
    pub skew_ms: f64,
    /// Store outage start, ms (outage only if `outage_to_ms > outage_from_ms`).
    pub outage_from_ms: f64,
    /// Store outage end, ms.
    pub outage_to_ms: f64,
    /// Deny when the store cannot answer (otherwise allow).
    pub fail_closed: bool,
    /// Share of requests sent to node 0, % (0 means spread evenly).
    pub hot_pct: u32,
    /// Seed for placement and jitter.
    pub seed: u32,
    /// How often to sample held tokens, ms.
    pub sample_ms: f64,
}

#[wasm_bindgen]
impl ClusterOptions {
    /// Defaults: adaptive leasing on 3 nodes, 100 per second, 250 µs delay + 250 µs
    /// jitter, no skew, no outage, fail open.
    #[wasm_bindgen(constructor)]
    pub fn new() -> ClusterOptions {
        ClusterOptions::default()
    }
}

impl Default for ClusterOptions {
    fn default() -> Self {
        Self {
            strategy: "adaptive_leasing".to_owned(),
            nodes: 3,
            limit: 100,
            window_ms: 1_000.0,
            delay_us: 250.0,
            jitter_us: 250.0,
            skew_ms: 0.0,
            outage_from_ms: 0.0,
            outage_to_ms: 0.0,
            fail_closed: false,
            hot_pct: 0,
            seed: 1,
            sample_ms: 50.0,
        }
    }
}

/// Everything a cluster run produced, as flat arrays for drawing.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterView {
    at_ms: Vec<f64>,
    node: Vec<u8>,
    allowed: Vec<u8>,
    ideal: Vec<u8>,
    wait_us: Vec<f64>,
    sample_at_ms: Vec<f64>,
    /// Row-major: sample × node.
    held: Vec<u32>,
    msg_at_ms: Vec<f64>,
    msg_node: Vec<u8>,
    /// 0 check, 1 lease, 2 return, 3 sync.
    msg_kind: Vec<u8>,
    msg_lost: Vec<u8>,
    msg_granted: Vec<u32>,
    nodes: u32,
    over_pct: f64,
    under_pct: f64,
    calls_per_1k: f64,
    p50_us: f64,
    p99_us: f64,
    admitted: u32,
    ideal_admitted: u32,
}

fn strategy(name: &str, config: ratelab::RateLimitConfig, nodes: u64) -> Result<Strategy, String> {
    if name == "central_exact_node_clock" {
        return Ok(Strategy::CentralExact { node_clock: true });
    }
    Strategy::defaults(config, nodes)
        .into_iter()
        .find(|s| s.name() == name)
        .ok_or_else(|| format!("unknown strategy {name:?}"))
}

impl ClusterView {
    /// Runs the cluster.
    pub fn build(options: &ClusterOptions, traffic: &Traffic) -> Result<ClusterView, String> {
        let config = config(options.limit, options.window_ms)?;
        let nodes = options.nodes.clamp(1, 16) as usize;
        let strategy = strategy(&options.strategy, config, nodes as u64)?;
        if traffic.len() > 500_000 {
            return Err("too many requests for the browser (max 500,000)".to_owned());
        }
        let mut c = ClusterConfig::new(nodes);
        c.one_way_delay = Duration::from_nanos(ms_to_nanos(options.delay_us / 1_000.0));
        c.jitter = Duration::from_nanos(ms_to_nanos(options.jitter_us / 1_000.0));
        c.seed = u64::from(options.seed);
        c.record_messages = true;
        c.sample_every = Some(Duration::from_nanos(
            ms_to_nanos(options.sample_ms).max(1_000_000),
        ));
        let skew_ns = (options.skew_ms * 1e6).round() as i64;
        c.skew_ns = (0..nodes as i64).map(|i| i * skew_ns).collect();
        if options.outage_to_ms > options.outage_from_ms {
            c.outage = Some((
                Nanos(ms_to_nanos(options.outage_from_ms)),
                Nanos(ms_to_nanos(options.outage_to_ms)),
            ));
        }
        let hot = (options.hot_pct > 0).then_some(u64::from(options.hot_pct.min(100)));
        let placed = place(&traffic.arrivals, nodes, hot, u64::from(options.seed));
        let policy = if options.fail_closed {
            FailurePolicy::FailClosed
        } else {
            FailurePolicy::FailOpen
        };
        let run = run_cluster(strategy, config, policy, &c, &placed);
        let ideal = metrics::ideal(config, &placed);
        let m = metrics::measure(&run, &ideal, config);

        let ms = |n: Nanos| n.0 as f64 / 1e6;
        Ok(ClusterView {
            at_ms: run.outcomes.iter().map(|o| ms(o.arrival.at)).collect(),
            node: run.outcomes.iter().map(|o| o.node as u8).collect(),
            allowed: run
                .outcomes
                .iter()
                .map(|o| u8::from(o.decision.allowed))
                .collect(),
            ideal: ideal.iter().map(|&b| u8::from(b)).collect(),
            wait_us: run
                .outcomes
                .iter()
                .map(|o| o.latency().as_nanos() as f64 / 1e3)
                .collect(),
            sample_at_ms: run.samples.iter().map(|s| ms(s.at)).collect(),
            held: run
                .samples
                .iter()
                .flat_map(|s| {
                    s.tokens
                        .iter()
                        .map(|&t| u32::try_from(t).unwrap_or(u32::MAX))
                })
                .collect(),
            msg_at_ms: run.messages.iter().map(|m| ms(m.sent_at)).collect(),
            msg_node: run.messages.iter().map(|m| m.node as u8).collect(),
            msg_kind: run
                .messages
                .iter()
                .map(|m| match m.op {
                    StoreOp::Check { .. } => 0,
                    StoreOp::Lease { .. } => 1,
                    StoreOp::Return { .. } => 2,
                    StoreOp::Sync { .. } => 3,
                })
                .collect(),
            msg_lost: run
                .messages
                .iter()
                .map(|m| u8::from(m.handled.is_none()))
                .collect(),
            msg_granted: run
                .messages
                .iter()
                .map(|m| match m.handled {
                    Some((_, StoreReply::Leased { granted, .. })) => {
                        u32::try_from(granted).unwrap_or(u32::MAX)
                    }
                    _ => 0,
                })
                .collect(),
            nodes: nodes as u32,
            over_pct: m.over_pct,
            under_pct: m.under_pct,
            calls_per_1k: m.calls_per_1k,
            p50_us: m.p50_us,
            p99_us: m.p99_us,
            admitted: run.outcomes.iter().filter(|o| o.decision.allowed).count() as u32,
            ideal_admitted: ideal.iter().filter(|&&b| b).count() as u32,
        })
    }
}

macro_rules! getters {
    ($($field:ident: $ty:ty => $js:literal),* $(,)?) => {
        #[wasm_bindgen]
        impl ClusterView {
            $(
                #[doc = concat!("`", stringify!($field), "`")]
                #[wasm_bindgen(js_name = $js)]
                pub fn $field(&self) -> $ty {
                    self.$field.clone()
                }
            )*
        }
    };
}

getters! {
    at_ms: Vec<f64> => "atMs",
    node: Vec<u8> => "node",
    allowed: Vec<u8> => "allowed",
    ideal: Vec<u8> => "ideal",
    wait_us: Vec<f64> => "waitUs",
    sample_at_ms: Vec<f64> => "sampleAtMs",
    held: Vec<u32> => "held",
    msg_at_ms: Vec<f64> => "msgAtMs",
    msg_node: Vec<u8> => "msgNode",
    msg_kind: Vec<u8> => "msgKind",
    msg_lost: Vec<u8> => "msgLost",
    msg_granted: Vec<u32> => "msgGranted",
    nodes: u32 => "nodes",
    over_pct: f64 => "overPct",
    under_pct: f64 => "underPct",
    calls_per_1k: f64 => "callsPer1k",
    p50_us: f64 => "p50Us",
    p99_us: f64 => "p99Us",
    admitted: u32 => "admitted",
    ideal_admitted: u32 => "idealAdmitted",
}

#[wasm_bindgen]
impl ClusterView {
    /// Runs `traffic` through a cluster. Throws on bad options.
    pub fn run(options: &ClusterOptions, traffic: &Traffic) -> Result<ClusterView, JsError> {
        ClusterView::build(options, traffic).map_err(|e| JsError::new(&e))
    }
}

/// Names of the five coordination strategies.
#[wasm_bindgen]
pub fn strategies() -> Vec<String> {
    STRATEGIES.iter().map(|&s| s.to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn traffic() -> Traffic {
        Traffic::build("steady", 2, 5.0, 100, 1_000.0, 1, 2.0).unwrap()
    }

    #[test]
    fn matches_the_bench_metrics() {
        // Same code path as the bench: central exact with no delay is the ideal.
        let opts = ClusterOptions {
            strategy: "central_exact".to_owned(),
            delay_us: 0.0,
            jitter_us: 0.0,
            ..ClusterOptions::default()
        };
        let v = ClusterView::build(&opts, &traffic()).unwrap();
        assert_eq!(v.allowed, v.ideal);
        assert_eq!(v.over_pct, 0.0);
        assert_eq!(v.calls_per_1k, 1_000.0);
    }

    #[test]
    fn every_strategy_runs_and_samples_every_node() {
        for s in STRATEGIES {
            let opts = ClusterOptions {
                strategy: s.to_owned(),
                ..ClusterOptions::default()
            };
            let v = ClusterView::build(&opts, &traffic()).unwrap();
            assert_eq!(v.at_ms.len(), traffic().len(), "{s}");
            assert_eq!(v.held.len(), v.sample_at_ms.len() * 3, "{s}");
            assert_eq!(v.msg_at_ms.len(), v.msg_kind.len(), "{s}");
        }
    }

    #[test]
    fn an_outage_loses_messages() {
        let opts = ClusterOptions {
            strategy: "central_exact".to_owned(),
            outage_from_ms: 1_000.0,
            outage_to_ms: 2_000.0,
            fail_closed: true,
            ..ClusterOptions::default()
        };
        let v = ClusterView::build(&opts, &traffic()).unwrap();
        assert!(v.msg_lost.contains(&1));
    }

    #[test]
    fn rejects_unknown_strategies() {
        let opts = ClusterOptions {
            strategy: "magic".to_owned(),
            ..ClusterOptions::default()
        };
        assert!(ClusterView::build(&opts, &traffic()).is_err());
    }
}
