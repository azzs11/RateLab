//! Local split: each node enforces `limit / N` on its own.

use super::bucket::ShareBucket;
use super::{CoordinationStrategy, MessageId, Outbox, RequestId, StoreReply};
use crate::{Cost, Nanos, RateLimitConfig};
use std::collections::HashMap;

/// Each of `N` nodes runs its own token bucket with `limit / N` of the capacity and
/// refill rate, and never talks to the store. Exact and free when traffic is spread
/// evenly; under-admits when one node gets more than its share.
///
/// ```
/// use drl_core::strategies::{CoordinationStrategy, LocalSplit, Outbox};
/// use drl_core::{Cost, Nanos, RateLimitConfig};
///
/// let mut node = LocalSplit::new(RateLimitConfig::per_second(10)?, 2);
/// let mut out = Outbox::default();
/// for id in 0..6 {
///     node.on_request(Nanos::ZERO, id, "k", Cost::ONE, &mut out);
/// }
/// assert_eq!(out.decisions.iter().filter(|(_, d)| d.allowed).count(), 5);
/// assert!(out.messages.is_empty());
/// # Ok::<(), drl_core::ConfigError>(())
/// ```
#[derive(Debug)]
pub struct LocalSplit {
    config: RateLimitConfig,
    nodes: u64,
    buckets: HashMap<String, ShareBucket>,
}

impl LocalSplit {
    /// One node's `1 / nodes` share of `config`.
    pub fn new(config: RateLimitConfig, nodes: u64) -> Self {
        Self {
            config,
            nodes: nodes.max(1),
            buckets: HashMap::new(),
        }
    }
}

impl CoordinationStrategy for LocalSplit {
    fn name(&self) -> &'static str {
        "local_split"
    }

    fn on_request(&mut self, now: Nanos, id: RequestId, key: &str, cost: Cost, out: &mut Outbox) {
        let (config, nodes) = (self.config, self.nodes);
        let bucket = self
            .buckets
            .entry(key.to_owned())
            .or_insert_with(|| ShareBucket::new(config, nodes, now));
        bucket.refill(now);
        let allowed = bucket.try_take(cost.0);
        out.decisions.push((id, bucket.decision(allowed, cost.0)));
    }

    fn on_reply(&mut self, _: Nanos, _: MessageId, _: StoreReply, _: &mut Outbox) {}

    fn on_failure(&mut self, _: Nanos, _: MessageId, _: &mut Outbox) {}
}
