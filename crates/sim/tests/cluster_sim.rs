//! The simulator's mechanics: delay, jitter, timeouts, outages and clock skew.

use ratelab::strategies::Strategy;
use ratelab::{Cost, FailurePolicy, Nanos, RateLimitConfig};
use sim::Arrival;
use sim::cluster::{ClusterArrival, ClusterConfig, ClusterRun, Outcome, run_cluster};
use std::time::Duration;

const MS: u64 = 1_000_000;
const CENTRAL: Strategy = Strategy::CentralExact { node_clock: false };

fn every(n: u64, gap_ns: u64, nodes: usize) -> Vec<ClusterArrival> {
    (0..n)
        .map(|i| ClusterArrival {
            arrival: Arrival {
                at: Nanos(i * gap_ns),
                key: 0,
                cost: Cost::ONE,
            },
            node: (i as usize) % nodes,
        })
        .collect()
}

fn run(
    strategy: Strategy,
    limit: u64,
    cluster: &ClusterConfig,
    a: &[ClusterArrival],
) -> ClusterRun {
    let config = RateLimitConfig::per_second(limit).unwrap();
    run_cluster(strategy, config, FailurePolicy::FailClosed, cluster, a)
}

#[test]
fn delay_adds_one_round_trip_per_store_call() {
    let mut c = ClusterConfig::new(2);
    c.one_way_delay = Duration::from_millis(1);
    let r = run(CENTRAL, 1_000, &c, &every(50, MS, 2));
    assert_eq!(r.undecided, 0);
    assert!(
        r.outcomes
            .iter()
            .all(|o| o.latency() == Duration::from_millis(2))
    );
    assert_eq!(r.store_ops, 50);
}

#[test]
fn jitter_is_seeded() {
    let mut c = ClusterConfig::new(3);
    c.one_way_delay = Duration::from_micros(200);
    c.jitter = Duration::from_micros(300);
    let a = every(300, 100_000, 3);
    c.seed = 1;
    let first = run(CENTRAL, 100, &c, &a);
    assert_eq!(first, run(CENTRAL, 100, &c, &a));
    c.seed = 2;
    assert_ne!(first, run(CENTRAL, 100, &c, &a));
}

#[test]
fn a_reply_later_than_the_timeout_fails_but_the_store_still_applied_it() {
    let mut c = ClusterConfig::new(1);
    c.one_way_delay = Duration::from_millis(60); // 120 ms round trip
    c.timeout = Duration::from_millis(100);
    let r = run(CENTRAL, 1_000, &c, &every(10, MS, 1));
    assert_eq!(r.timeouts, 10);
    assert_eq!(r.store_ops, 10);
    assert!(r.outcomes.iter().all(|o| !o.decision.allowed)); // fail closed
    assert!(
        r.outcomes
            .iter()
            .all(|o| o.latency() == Duration::from_millis(100))
    );
}

#[test]
fn messages_during_an_outage_are_lost() {
    let mut c = ClusterConfig::new(1);
    c.outage = Some((Nanos(10 * MS), Nanos(20 * MS)));
    let r = run(CENTRAL, 1_000, &c, &every(30, MS, 1));
    let in_outage = |o: &Outcome| (10 * MS..20 * MS).contains(&o.arrival.at.0);
    assert_eq!(r.outcomes.iter().filter(|o| in_outage(o)).count(), 10);
    assert!(
        r.outcomes
            .iter()
            .filter(|o| in_outage(o))
            .all(|o| !o.decision.allowed)
    );
    assert!(
        r.outcomes
            .iter()
            .filter(|o| !in_outage(o))
            .all(|o| o.decision.allowed)
    );
    assert_eq!(r.timeouts, 10);
    assert_eq!(r.store_ops, 20);
}

#[test]
fn skewed_clocks_matter_only_when_nodes_send_their_time() {
    // Limit 1 per second; two nodes alternate every 100 ms; node 1 runs 900 ms ahead.
    let mut c = ClusterConfig::new(2);
    c.skew_ns = vec![0, 900 * MS as i64];
    let a = every(40, 100 * MS, 2);
    let store_clock = run(CENTRAL, 1, &c, &a);
    let node_clock = run(Strategy::CentralExact { node_clock: true }, 1, &c, &a);
    let admitted = |r: &ClusterRun| r.outcomes.iter().filter(|o| o.decision.allowed).count();
    assert_eq!(admitted(&store_clock), 4); // 1 per second over 4 s
    assert!(admitted(&node_clock) > admitted(&store_clock));
}

#[test]
fn ticks_stop_after_the_traffic() {
    let config = RateLimitConfig::per_second(10).unwrap();
    let r = run_cluster(
        Strategy::AsyncSync {
            interval: Duration::from_millis(1),
        },
        config,
        FailurePolicy::FailOpen,
        &ClusterConfig::new(2),
        &every(5, MS, 2),
    );
    assert_eq!(r.undecided, 0);
    assert!(r.messages_sent < 10);
}

#[test]
fn the_message_log_records_requests_and_replies() {
    let mut c = ClusterConfig::new(1);
    c.record_messages = true;
    c.outage = Some((Nanos(5 * MS), Nanos(6 * MS)));
    let r = run(CENTRAL, 1_000, &c, &every(10, MS, 1));
    assert_eq!(r.messages.len(), 10);
    assert_eq!(r.messages.iter().filter(|m| m.handled.is_none()).count(), 1);
    assert!(r.messages.iter().all(|m| m.node == 0));
    // Off by default.
    let quiet = run(CENTRAL, 1_000, &ClusterConfig::new(1), &every(10, MS, 1));
    assert!(quiet.messages.is_empty());
}

#[test]
fn samples_show_what_each_node_holds() {
    let mut c = ClusterConfig::new(2);
    c.sample_every = Some(Duration::from_millis(100));
    let r = run(Strategy::LocalSplit, 10, &c, &every(4, MS, 2));
    // The first sample comes right after the request at t = 0: node 0 has spent one of
    // its 5 tokens, node 1 has no bucket yet. Later each node refills its full share.
    assert_eq!(r.samples[0].tokens, [4, 0]);
    assert!(r.samples.len() > 5);
    assert_eq!(r.samples.last().unwrap().tokens, [5, 5]); // a full 10 / 2 share each
}
