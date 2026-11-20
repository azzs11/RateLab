//! A deterministic discrete-event simulation of N nodes sharing one store.
//!
//! Every node runs a [`CoordinationStrategy`]; they exchange messages with a
//! [`Store`] over a simulated network. Everything is driven by one event queue ordered
//! by `(time, sequence number)`, and the only randomness (network jitter) comes from a
//! seeded [`SplitMix64`], so a run is reproducible bit for bit.
//!
//! - **Delay:** each message takes `one_way_delay` plus uniform jitter in `0..jitter`.
//! - **Timeouts:** a node gives up on a message after `timeout`; if the reply would be
//!   later, the store has still applied the operation (as with a real Redis).
//! - **Outage:** messages that would reach the store during `outage` are lost.
//! - **Clock skew:** node `i` reads `store time + skew[i]`. The store keeps its own
//!   clock, like Redis `TIME`.
//!
//! ```
//! use drl_core::strategies::Strategy;
//! use drl_core::{Cost, FailurePolicy, Nanos, RateLimitConfig};
//! use sim::cluster::{ClusterArrival, ClusterConfig, run_cluster};
//! use sim::Arrival;
//!
//! let config = RateLimitConfig::per_second(2)?;
//! let arrivals: Vec<ClusterArrival> = (0..3)
//!     .map(|i| ClusterArrival {
//!         arrival: Arrival { at: Nanos(i), key: 0, cost: Cost::ONE },
//!         node: i as usize % 2,
//!     })
//!     .collect();
//! let run = run_cluster(
//!     Strategy::CentralExact { node_clock: false },
//!     config,
//!     FailurePolicy::FailClosed,
//!     &ClusterConfig::new(2),
//!     &arrivals,
//! );
//! let allowed: Vec<bool> = run.outcomes.iter().map(|o| o.decision.allowed).collect();
//! assert_eq!(allowed, [true, true, false]); // one shared limit of 2
//! assert_eq!(run.store_ops, 3);
//! # Ok::<(), drl_core::ConfigError>(())
//! ```

use crate::{Arrival, SplitMix64};
use drl_core::strategies::{
    CoordinationStrategy, Message, MessageId, Outbox, Store, StoreOp, StoreReply, Strategy,
};
use drl_core::{Decision, FailurePolicy, Nanos, RateLimitConfig};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::time::Duration;

/// Network and failure settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterConfig {
    /// Nodes.
    pub nodes: usize,
    /// Fixed one-way delay between a node and the store.
    pub one_way_delay: Duration,
    /// Extra one-way delay, uniform in `0..jitter`.
    pub jitter: Duration,
    /// How long a node waits for a reply.
    pub timeout: Duration,
    /// Clock offset of each node in nanoseconds (missing entries are 0).
    pub skew_ns: Vec<i64>,
    /// While the store is unreachable, as `(start, end)` in simulation time.
    pub outage: Option<(Nanos, Nanos)>,
    /// Seed for the jitter.
    pub seed: u64,
    /// Keep a log of every message to the store (for visualisation).
    pub record_messages: bool,
}

impl ClusterConfig {
    /// `nodes` nodes with no delay, no skew and no outage, and a 100 ms timeout.
    pub fn new(nodes: usize) -> Self {
        Self {
            nodes: nodes.max(1),
            one_way_delay: Duration::ZERO,
            jitter: Duration::ZERO,
            timeout: Duration::from_millis(100),
            skew_ns: Vec::new(),
            outage: None,
            seed: 0,
            record_messages: false,
        }
    }
}

/// A request and the node it is sent to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterArrival {
    /// The request (its time is simulation time).
    pub arrival: Arrival,
    /// Index of the node that receives it.
    pub node: usize,
}

/// What happened to one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// The request.
    pub arrival: Arrival,
    /// The node that decided it.
    pub node: usize,
    /// When it was decided (simulation time).
    pub decided_at: Nanos,
    /// The decision.
    pub decision: Decision,
}

impl Outcome {
    /// Time the request waited for its decision.
    pub fn latency(&self) -> Duration {
        self.decided_at.saturating_since(self.arrival.at)
    }
}

/// One message from a node to the store, when [`ClusterConfig::record_messages`] is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageRecord {
    /// When the node sent it (simulation time).
    pub sent_at: Nanos,
    /// The node that sent it.
    pub node: usize,
    /// What it asked for.
    pub op: StoreOp,
    /// When the store applied it and what it answered; `None` if it was lost.
    pub handled: Option<(Nanos, StoreReply)>,
}

/// Everything one run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterRun {
    /// One entry per decided request, in arrival order.
    pub outcomes: Vec<Outcome>,
    /// Requests that never got a decision (a strategy bug; always 0 for the built-in ones).
    pub undecided: usize,
    /// Messages nodes sent towards the store.
    pub messages_sent: u64,
    /// Operations the store applied.
    pub store_ops: u64,
    /// Messages that timed out.
    pub timeouts: u64,
    /// Every message, if recorded.
    pub messages: Vec<MessageRecord>,
}

#[derive(Debug)]
enum Event {
    Arrive(usize),
    AtStore {
        node: usize,
        msg: Message,
        sent: u64,
        record: Option<usize>,
    },
    AtNode {
        node: usize,
        id: MessageId,
        reply: StoreReply,
    },
    Timeout {
        node: usize,
        id: MessageId,
    },
    Tick {
        node: usize,
    },
}

#[derive(Debug)]
struct Queued {
    at: u64,
    seq: u64,
    event: Event,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        (self.at, self.seq) == (other.at, other.seq)
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Queued {
    // Reversed: BinaryHeap is a max-heap and we want the earliest event first.
    fn cmp(&self, other: &Self) -> Ordering {
        (other.at, other.seq).cmp(&(self.at, self.seq))
    }
}

/// Simulation time is offset so that negative clock skew never reads before zero.
const EPOCH: u64 = 1_000_000_000_000;

struct Sim<'a> {
    cfg: &'a ClusterConfig,
    queue: BinaryHeap<Queued>,
    seq: u64,
    rng: SplitMix64,
    store: Store,
    nodes: Vec<Box<dyn CoordinationStrategy>>,
    arrivals: &'a [ClusterArrival],
    outcomes: Vec<Option<Outcome>>,
    messages_sent: u64,
    timeouts: u64,
    out: Outbox,
    log: Vec<MessageRecord>,
}

impl Sim<'_> {
    fn push(&mut self, at: u64, event: Event) {
        self.seq += 1;
        self.queue.push(Queued {
            at,
            seq: self.seq,
            event,
        });
    }

    fn node_time(&self, node: usize, t: u64) -> Nanos {
        let skew = self.cfg.skew_ns.get(node).copied().unwrap_or(0);
        Nanos(EPOCH.saturating_add_signed(skew).saturating_add(t))
    }

    fn delay(&mut self) -> u64 {
        let base = Nanos::from_duration(self.cfg.one_way_delay).0;
        let jitter = Nanos::from_duration(self.cfg.jitter).0;
        base + self.rng.below(jitter)
    }

    fn in_outage(&self, t: u64) -> bool {
        self.cfg
            .outage
            .is_some_and(|(start, end)| (start.0..end.0).contains(&t))
    }

    /// Records decisions and sends messages from the outbox.
    fn flush(&mut self, node: usize, now: u64) {
        let mut out = std::mem::take(&mut self.out);
        for (id, decision) in out.decisions.drain(..) {
            let index = id as usize;
            if let Some(slot) = self.outcomes.get_mut(index)
                && slot.is_none()
                && let Some(a) = self.arrivals.get(index)
            {
                *slot = Some(Outcome {
                    arrival: a.arrival,
                    node,
                    decided_at: Nanos(now),
                    decision,
                });
            }
        }
        for msg in out.messages.drain(..) {
            self.messages_sent += 1;
            let record = self.cfg.record_messages.then(|| {
                self.log.push(MessageRecord {
                    sent_at: Nanos(now),
                    node,
                    op: msg.op,
                    handled: None,
                });
                self.log.len() - 1
            });
            let arrive = now + self.delay();
            self.push(
                arrive,
                Event::AtStore {
                    node,
                    msg,
                    sent: now,
                    record,
                },
            );
        }
        self.out = out;
    }

    fn step(&mut self, q: Queued, tick_until: u64) {
        let now = q.at;
        match q.event {
            Event::Arrive(i) => {
                let Some(a) = self.arrivals.get(i).copied() else {
                    return;
                };
                let node = a.node.min(self.nodes.len() - 1);
                let t = self.node_time(node, now);
                let key = format!("k{}", a.arrival.key);
                self.nodes[node].on_request(t, i as u64, &key, a.arrival.cost, &mut self.out);
                self.flush(node, now);
            }
            Event::AtStore {
                node,
                msg,
                sent,
                record,
            } => {
                let timeout_at = sent + Nanos::from_duration(self.cfg.timeout).0;
                if self.in_outage(now) {
                    self.push(timeout_at, Event::Timeout { node, id: msg.id });
                    return;
                }
                let reply = self.store.handle(Nanos(EPOCH + now), &msg.key, &msg.op);
                if let Some(entry) = record.and_then(|i| self.log.get_mut(i)) {
                    entry.handled = Some((Nanos(now), reply));
                }
                let back = now + self.delay();
                if back > timeout_at {
                    self.push(timeout_at, Event::Timeout { node, id: msg.id });
                } else {
                    self.push(
                        back,
                        Event::AtNode {
                            node,
                            id: msg.id,
                            reply,
                        },
                    );
                }
            }
            Event::AtNode { node, id, reply } => {
                let t = self.node_time(node, now);
                self.nodes[node].on_reply(t, id, reply, &mut self.out);
                self.flush(node, now);
            }
            Event::Timeout { node, id } => {
                self.timeouts += 1;
                let t = self.node_time(node, now);
                self.nodes[node].on_failure(t, id, &mut self.out);
                self.flush(node, now);
            }
            Event::Tick { node } => {
                let t = self.node_time(node, now);
                self.nodes[node].on_tick(t, &mut self.out);
                self.flush(node, now);
                if let Some(every) = self.nodes[node].tick_every() {
                    let next = now + Nanos::from_duration(every).0.max(1);
                    if next <= tick_until {
                        self.push(next, Event::Tick { node });
                    }
                }
            }
        }
    }
}

/// Runs `arrivals` (sorted by time) through `nodes` instances of `strategy`.
///
/// Ticks continue for one window after the last arrival so that pending work
/// (syncs, returns) completes.
pub fn run_cluster(
    strategy: Strategy,
    config: RateLimitConfig,
    policy: FailurePolicy,
    cluster: &ClusterConfig,
    arrivals: &[ClusterArrival],
) -> ClusterRun {
    let n = cluster.nodes.max(1);
    let mut sim = Sim {
        cfg: cluster,
        queue: BinaryHeap::with_capacity(arrivals.len() * 2),
        seq: 0,
        rng: SplitMix64::new(cluster.seed),
        store: Store::new(config),
        nodes: (0..n)
            .map(|_| strategy.build(config, n as u64, policy))
            .collect(),
        arrivals,
        outcomes: vec![None; arrivals.len()],
        messages_sent: 0,
        timeouts: 0,
        out: Outbox::default(),
        log: Vec::new(),
    };
    for (i, a) in arrivals.iter().enumerate() {
        sim.push(a.arrival.at.0, Event::Arrive(i));
    }
    let last = arrivals.last().map_or(0, |a| a.arrival.at.0);
    let tick_until = last.saturating_add(config.window_nanos());
    for node in 0..n {
        if let Some(every) = sim.nodes[node].tick_every() {
            sim.push(Nanos::from_duration(every).0.max(1), Event::Tick { node });
        }
    }
    while let Some(q) = sim.queue.pop() {
        sim.step(q, tick_until);
    }
    let undecided = sim.outcomes.iter().filter(|o| o.is_none()).count();
    ClusterRun {
        outcomes: sim.outcomes.into_iter().flatten().collect(),
        undecided,
        messages_sent: sim.messages_sent,
        store_ops: sim.store.ops(),
        timeouts: sim.timeouts,
        messages: sim.log,
    }
}

/// Assigns each arrival a node: uniformly, or with `hot_pct` % going to node 0 and the
/// rest spread evenly over the others. Deterministic for a seed.
///
/// ```
/// use std::time::Duration;
///
/// let a = sim::poisson(1, Duration::from_secs(1), Duration::from_millis(1), 2);
/// let placed = sim::cluster::place(&a, 4, Some(80), 9);
/// let on_0 = placed.iter().filter(|p| p.node == 0).count() as f64 / placed.len() as f64;
/// assert!((0.75..0.85).contains(&on_0));
/// ```
pub fn place(
    arrivals: &[Arrival],
    nodes: usize,
    hot_pct: Option<u64>,
    seed: u64,
) -> Vec<ClusterArrival> {
    let nodes = nodes.max(1);
    let mut rng = SplitMix64::new(seed ^ 0x9E37_79B9);
    arrivals
        .iter()
        .map(|&arrival| {
            let node = match hot_pct {
                Some(p) if nodes > 1 => {
                    if rng.below(100) < p {
                        0
                    } else {
                        1 + rng.below(nodes as u64 - 1) as usize
                    }
                }
                _ => rng.below(nodes as u64) as usize,
            };
            ClusterArrival { arrival, node }
        })
        .collect()
}
