//! The behaviour contract every coordination strategy must meet, run against all five
//! in the simulator.

use ratelab::strategies::Strategy;
use ratelab::{Algorithm, Cost, FailurePolicy, ManualClock, Nanos, RateLimitConfig};
use sim::cluster::{ClusterArrival, ClusterConfig, ClusterRun, place, run_cluster};
use std::sync::Arc;
use std::time::Duration;

fn config() -> RateLimitConfig {
    RateLimitConfig::per_second(100).unwrap()
}

/// Poisson traffic at 2× the limit on 5 keys for 5 s.
fn traffic(seed: u64, nodes: usize, hot: Option<u64>) -> Vec<ClusterArrival> {
    let a = sim::poisson(
        seed,
        Duration::from_secs(5),
        Duration::from_micros(1_000),
        5,
    );
    place(&a, nodes, hot, seed)
}

fn realistic(nodes: usize) -> ClusterConfig {
    let mut c = ClusterConfig::new(nodes);
    c.one_way_delay = Duration::from_micros(250);
    c.jitter = Duration::from_micros(250);
    c.seed = 7;
    c
}

fn run(s: Strategy, c: &ClusterConfig, a: &[ClusterArrival]) -> ClusterRun {
    run_cluster(s, config(), FailurePolicy::FailClosed, c, a)
}

/// Decisions of one perfectly coordinated token bucket on the same arrival times.
fn ideal(a: &[ClusterArrival]) -> Vec<bool> {
    let clock = Arc::new(ManualClock::new());
    let bucket = Algorithm::TokenBucket.build(config(), Arc::clone(&clock));
    let arrivals: Vec<_> = a.iter().map(|c| c.arrival).collect();
    sim::run(&bucket, &clock, &arrivals)
        .steps
        .iter()
        .map(|s| s.decision.allowed)
        .collect()
}

fn admitted(r: &ClusterRun) -> u64 {
    r.outcomes
        .iter()
        .filter(|o| o.decision.allowed)
        .map(|o| o.arrival.cost.0)
        .sum()
}

#[test]
fn every_request_is_decided_exactly_once() {
    for nodes in [1, 3, 8] {
        let a = traffic(1, nodes, Some(80));
        let mut c = realistic(nodes);
        c.outage = Some((Nanos(2_000_000_000), Nanos(3_000_000_000)));
        for s in Strategy::defaults(config(), nodes as u64) {
            let r = run(s, &c, &a);
            assert_eq!(r.undecided, 0, "{} on {nodes} nodes", s.name());
            assert_eq!(r.outcomes.len(), a.len(), "{}", s.name());
        }
    }
}

#[test]
fn runs_are_deterministic() {
    let a = traffic(2, 3, None);
    for s in Strategy::defaults(config(), 3) {
        assert_eq!(
            run(s, &realistic(3), &a),
            run(s, &realistic(3), &a),
            "{}",
            s.name()
        );
    }
}

#[test]
fn with_no_delay_central_exact_is_the_ideal_bucket() {
    for nodes in [1, 3] {
        let a = traffic(3, nodes, None);
        let r = run(
            Strategy::CentralExact { node_clock: false },
            &ClusterConfig::new(nodes),
            &a,
        );
        let got: Vec<bool> = r.outcomes.iter().map(|o| o.decision.allowed).collect();
        assert_eq!(got, ideal(&a));
    }
}

#[test]
fn local_split_on_one_node_is_the_ideal_bucket() {
    let a = traffic(4, 1, None);
    let r = run(Strategy::LocalSplit, &ClusterConfig::new(1), &a);
    let got: Vec<bool> = r.outcomes.iter().map(|o| o.decision.allowed).collect();
    assert_eq!(got, ideal(&a));
}

#[test]
fn strategies_that_take_tokens_from_the_store_never_exceed_the_global_budget() {
    // Per key, a token bucket hands out at most its capacity plus its refill: 100 at the
    // start + 100/s for 5 s + 1 s of trailing ticks. The traffic has 5 keys.
    let budget = 5 * (100 + 100 * 5 + 100);
    for nodes in [1, 3, 8] {
        let a = traffic(5, nodes, Some(80));
        for s in Strategy::defaults(config(), nodes as u64) {
            let r = run(s, &realistic(nodes), &a);
            let limit = match s {
                Strategy::CentralExact { .. }
                | Strategy::FixedLeasing { .. }
                | Strategy::AdaptiveLeasing(_) => budget,
                // Each node can spend the same synced remainder.
                Strategy::AsyncSync { .. } => budget * nodes as u64,
                Strategy::LocalSplit => budget,
            };
            assert!(
                admitted(&r) <= limit,
                "{} on {nodes} nodes: {}",
                s.name(),
                admitted(&r)
            );
        }
    }
}

#[test]
fn local_split_under_admits_when_traffic_is_skewed() {
    let a = traffic(6, 4, Some(90));
    let r = run(Strategy::LocalSplit, &ClusterConfig::new(4), &a);
    let ideal_admitted = ideal(&a).iter().filter(|&&x| x).count() as u64;
    assert!(
        admitted(&r) < ideal_admitted * 6 / 10,
        "{} vs {ideal_admitted}",
        admitted(&r)
    );
}

#[test]
fn leasing_needs_fewer_store_calls_than_central_exact() {
    let a = traffic(7, 3, None);
    let defaults = Strategy::defaults(config(), 3);
    let central = run(defaults[0], &realistic(3), &a);
    for s in &defaults[3..] {
        let r = run(*s, &realistic(3), &a);
        assert!(
            r.store_ops * 2 < central.store_ops,
            "{}: {} vs {}",
            s.name(),
            r.store_ops,
            central.store_ops
        );
    }
}

#[test]
fn during_an_outage_the_policy_decides_central_requests() {
    let a = traffic(8, 3, None);
    let mut c = realistic(3);
    let (start, end) = (2_000_000_000, 3_000_000_000);
    c.outage = Some((Nanos(start), Nanos(end)));
    for (policy, allowed) in [
        (FailurePolicy::FailOpen, true),
        (FailurePolicy::FailClosed, false),
    ] {
        let r = run_cluster(
            Strategy::CentralExact { node_clock: false },
            config(),
            policy,
            &c,
            &a,
        );
        // Requests well inside the outage cannot reach the store (near the end, a
        // message can arrive after the outage is over).
        let inside = r
            .outcomes
            .iter()
            .filter(|o| (start + 1_000_000..end - 1_000_000).contains(&o.arrival.at.0));
        assert!(inside.clone().count() > 0);
        assert!(
            inside.clone().all(|o| o.decision.allowed == allowed),
            "{policy:?}"
        );
    }
}

#[test]
fn costs_above_the_limit_are_never_admitted() {
    let a: Vec<ClusterArrival> = (0..20)
        .map(|i| ClusterArrival {
            arrival: sim::Arrival {
                at: Nanos(i * 1_000_000),
                key: 0,
                cost: Cost(101),
            },
            node: (i % 3) as usize,
        })
        .collect();
    for s in Strategy::defaults(config(), 3) {
        let r = run(s, &realistic(3), &a);
        assert_eq!(admitted(&r), 0, "{}", s.name());
    }
}
