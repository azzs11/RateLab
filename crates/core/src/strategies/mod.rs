//! Coordination strategies: how N nodes enforce one global limit together.
//!
//! Each node runs a [`CoordinationStrategy`]. It sees requests, decides some of them
//! locally, and exchanges [`StoreOp`] messages with a shared [`Store`] (what Redis
//! would run). Strategies do no I/O and read no clock: the caller passes the time and
//! delivers messages, so the same code runs in the deterministic simulator
//! (`crates/sim`), in tests, and later against a real Redis.
//!
//! The global rule is the same everywhere: a token bucket of `limit` per `window` per
//! key, as enforced by the store.
//!
//! | Strategy | Store traffic | Trade-off |
//! |---|---|---|
//! | [`CentralExact`] | one round trip per request | exact, but every request waits for the store |
//! | [`LocalSplit`] | none | each node gets `limit / N`; wrong when traffic is uneven |
//! | [`AsyncSync`] | one sync per active key per interval per node | no waiting; over-admits between syncs |

mod async_sync;
mod bucket;
mod central;
mod local_split;
mod store;

pub use async_sync::AsyncSync;
pub use bucket::ShareBucket;
pub use central::CentralExact;
pub use local_split::LocalSplit;
pub use store::Store;

use crate::breaker::FailurePolicy;
use crate::{Cost, Decision, Nanos, RateLimitConfig};
use std::time::Duration;

/// Identifies one request at a node, chosen by the caller.
pub type RequestId = u64;

/// Identifies one message from a node to the store, chosen by the strategy. The reply
/// (or failure) carries the same id.
pub type MessageId = u64;

/// What a node asks the store to do for one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOp {
    /// Check and charge `cost`. `at` is the node's clock if the node supplies the time;
    /// `None` means the store uses its own clock (Redis `TIME`).
    Check {
        /// Units to charge.
        cost: Cost,
        /// Time supplied by the node, if any.
        at: Option<Nanos>,
    },
    /// Take up to `want` tokens, to spend locally.
    Lease {
        /// Tokens wanted.
        want: u64,
    },
    /// Give back unused leased tokens.
    Return {
        /// Tokens returned.
        amount: u64,
    },
    /// Report tokens spent locally since the last sync, and ask what is left.
    Sync {
        /// Tokens spent.
        used: u64,
    },
}

/// The store's answer to a [`StoreOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreReply {
    /// Answer to [`StoreOp::Check`].
    Checked(Decision),
    /// Answer to [`StoreOp::Lease`]. When nothing was granted, `retry_after` is the
    /// time until one token is available.
    Leased {
        /// Tokens granted (possibly fewer than wanted, possibly zero).
        granted: u64,
        /// Time until a token is available, when none was granted.
        retry_after: Option<Duration>,
    },
    /// Answer to [`StoreOp::Return`].
    Returned,
    /// Answer to [`StoreOp::Sync`]: tokens left globally after applying the report.
    Synced {
        /// Whole tokens left.
        remaining: u64,
    },
}

/// A message to send to the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Id the reply will carry.
    pub id: MessageId,
    /// The key it concerns.
    pub key: String,
    /// What to do.
    pub op: StoreOp,
}

/// What a strategy wants done after handling an event.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outbox {
    /// Requests decided now.
    pub decisions: Vec<(RequestId, Decision)>,
    /// Messages to send to the store.
    pub messages: Vec<Message>,
}

impl Outbox {
    /// Empties the outbox, keeping its allocations.
    pub fn clear(&mut self) {
        self.decisions.clear();
        self.messages.clear();
    }
}

/// One node's side of a coordination strategy.
///
/// The caller delivers events in time order with the node's current time:
/// requests, store replies, store failures (timeouts, outages) and periodic ticks.
/// Every request must eventually get exactly one decision in some [`Outbox`].
pub trait CoordinationStrategy: Send {
    /// Short snake_case name.
    fn name(&self) -> &'static str;

    /// A request for `key` costing `cost` arrives.
    fn on_request(&mut self, now: Nanos, id: RequestId, key: &str, cost: Cost, out: &mut Outbox);

    /// The store answered message `id`.
    fn on_reply(&mut self, now: Nanos, id: MessageId, reply: StoreReply, out: &mut Outbox);

    /// Message `id` got no answer (timeout or outage).
    fn on_failure(&mut self, now: Nanos, id: MessageId, out: &mut Outbox);

    /// How often [`on_tick`](Self::on_tick) should run; `None` for never.
    fn tick_every(&self) -> Option<Duration> {
        None
    }

    /// Periodic work (syncing counts, returning idle leases).
    fn on_tick(&mut self, _now: Nanos, _out: &mut Outbox) {}
}

/// Names a strategy and its parameters, to build one per node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// [`CentralExact`]. `node_clock` sends the node's time instead of using the store's.
    CentralExact {
        /// Whether the node supplies the time.
        node_clock: bool,
    },
    /// [`LocalSplit`].
    LocalSplit,
    /// [`AsyncSync`], syncing every `interval`.
    AsyncSync {
        /// Time between syncs.
        interval: Duration,
    },
}

impl Strategy {
    /// Snake-case name.
    ///
    /// ```
    /// use drl_core::strategies::Strategy;
    /// assert_eq!(Strategy::LocalSplit.name(), "local_split");
    /// ```
    pub fn name(&self) -> &'static str {
        match self {
            Strategy::CentralExact { node_clock: false } => "central_exact",
            Strategy::CentralExact { node_clock: true } => "central_exact_node_clock",
            Strategy::LocalSplit => "local_split",
            Strategy::AsyncSync { .. } => "async_sync",
        }
    }

    /// One node's instance, for a cluster of `nodes` nodes enforcing `config`.
    ///
    /// ```
    /// use drl_core::strategies::{Outbox, Strategy};
    /// use drl_core::{Cost, FailurePolicy, Nanos, RateLimitConfig};
    ///
    /// let mut node = Strategy::LocalSplit.build(
    ///     RateLimitConfig::per_second(6)?,
    ///     3,
    ///     FailurePolicy::FailOpen,
    /// );
    /// let mut out = Outbox::default();
    /// for id in 0..3 {
    ///     node.on_request(Nanos::ZERO, id, "k", Cost::ONE, &mut out);
    /// }
    /// let allowed = out.decisions.iter().filter(|(_, d)| d.allowed).count();
    /// assert_eq!(allowed, 2); // this node's share of 6 across 3 nodes
    /// # Ok::<(), drl_core::ConfigError>(())
    /// ```
    pub fn build(
        &self,
        config: RateLimitConfig,
        nodes: u64,
        policy: FailurePolicy,
    ) -> Box<dyn CoordinationStrategy> {
        match *self {
            Strategy::CentralExact { node_clock } => {
                Box::new(CentralExact::new(policy, node_clock))
            }
            Strategy::LocalSplit => Box::new(LocalSplit::new(config, nodes)),
            Strategy::AsyncSync { interval } => Box::new(AsyncSync::new(config, nodes, interval)),
        }
    }
}
