//! Local decisions with periodic synchronisation of counts.

use super::bucket::ShareBucket;
use super::{CoordinationStrategy, Message, MessageId, Outbox, RequestId, StoreOp, StoreReply};
use crate::{Cost, Nanos, RateLimitConfig};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

#[derive(Debug)]
struct KeyState {
    /// This node's estimate of tokens it may still spend.
    estimate: ShareBucket,
    /// Tokens spent here since the last sync was sent.
    unsynced: u64,
    /// A sync for this key is waiting for its reply.
    in_flight: bool,
}

/// Decides every request locally against an estimate, and every `interval` reports
/// what it spent to the store, which answers with the global tokens left.
///
/// Between syncs the estimate refills at this node's `1 / N` share of the rate. After
/// a sync it is reset to the global remainder (minus what was spent while the sync was
/// in flight). All nodes see the same remainder, so in a busy window each can spend it:
/// over-admission is bounded by about `N - 1` times the remainder per interval. This is
/// the idea Cloudflare described for counting at its edge.
///
/// No request waits for the store, and a store outage only stops the resets: each node
/// keeps deciding on its own share.
///
/// ```
/// use drl_core::strategies::{AsyncSync, CoordinationStrategy, Outbox, Store};
/// use drl_core::{Cost, Nanos, RateLimitConfig};
/// use std::time::Duration;
///
/// let config = RateLimitConfig::per_second(10)?;
/// let mut node = AsyncSync::new(config, 2, Duration::from_millis(10));
/// let mut out = Outbox::default();
/// node.on_request(Nanos::ZERO, 1, "k", Cost::ONE, &mut out);
/// assert!(out.decisions[0].1.allowed); // decided locally, no message
/// assert!(out.messages.is_empty());
///
/// node.on_tick(Nanos(10_000_000), &mut out); // reports the 1 token spent
/// assert_eq!(out.messages.len(), 1);
/// # Ok::<(), drl_core::ConfigError>(())
/// ```
#[derive(Debug)]
pub struct AsyncSync {
    config: RateLimitConfig,
    nodes: u64,
    interval: Duration,
    /// Ordered, so syncs go out in the same order on every run.
    keys: BTreeMap<String, KeyState>,
    next: MessageId,
    pending: HashMap<MessageId, (String, u64)>,
}

impl AsyncSync {
    /// A node of a `nodes`-node cluster that syncs every `interval`.
    pub fn new(config: RateLimitConfig, nodes: u64, interval: Duration) -> Self {
        Self {
            config,
            nodes: nodes.max(1),
            interval,
            keys: BTreeMap::new(),
            next: 0,
            pending: HashMap::new(),
        }
    }
}

impl CoordinationStrategy for AsyncSync {
    fn name(&self) -> &'static str {
        "async_sync"
    }

    fn on_request(&mut self, now: Nanos, id: RequestId, key: &str, cost: Cost, out: &mut Outbox) {
        let (config, nodes) = (self.config, self.nodes);
        let state = self.keys.entry(key.to_owned()).or_insert_with(|| {
            // Holds up to the whole limit, refills at this node's share; starts with
            // the share until the first sync says more.
            let mut estimate = ShareBucket::with_shares(config, 1, nodes, now);
            estimate.set_tokens(config.limit() / nodes);
            KeyState {
                estimate,
                unsynced: 0,
                in_flight: false,
            }
        });
        state.estimate.refill(now);
        let allowed = state.estimate.try_take(cost.0);
        if allowed {
            state.unsynced += cost.0;
        }
        out.decisions
            .push((id, state.estimate.decision(allowed, cost.0)));
    }

    fn on_reply(&mut self, now: Nanos, id: MessageId, reply: StoreReply, _out: &mut Outbox) {
        let Some((key, _sent)) = self.pending.remove(&id) else {
            return;
        };
        if let (Some(state), StoreReply::Synced { remaining }) = (self.keys.get_mut(&key), reply) {
            state.in_flight = false;
            state.estimate.refill(now);
            state
                .estimate
                .set_tokens(remaining.saturating_sub(state.unsynced));
        }
    }

    fn on_failure(&mut self, _now: Nanos, id: MessageId, _out: &mut Outbox) {
        if let Some((key, sent)) = self.pending.remove(&id)
            && let Some(state) = self.keys.get_mut(&key)
        {
            // Not applied: report it again next time.
            state.in_flight = false;
            state.unsynced += sent;
        }
    }

    fn tick_every(&self) -> Option<Duration> {
        Some(self.interval)
    }

    fn local_tokens(&self, now: Nanos) -> u64 {
        self.keys.values().map(|k| k.estimate.tokens_at(now)).sum()
    }

    fn on_tick(&mut self, _now: Nanos, out: &mut Outbox) {
        for (key, state) in &mut self.keys {
            if state.unsynced == 0 || state.in_flight {
                continue;
            }
            let msg = self.next;
            self.next += 1;
            self.pending.insert(msg, (key.clone(), state.unsynced));
            out.messages.push(Message {
                id: msg,
                key: key.clone(),
                op: StoreOp::Sync {
                    used: state.unsynced,
                },
            });
            state.unsynced = 0;
            state.in_flight = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::Store;

    fn deliver(node: &mut AsyncSync, store: &mut Store, now: Nanos, out: &mut Outbox) {
        let messages = std::mem::take(&mut out.messages);
        for m in messages {
            let reply = store.handle(now, &m.key, &m.op);
            node.on_reply(now, m.id, reply, out);
        }
    }

    #[test]
    fn sync_reports_spending_and_resets_the_estimate() {
        let config = RateLimitConfig::new(100, Duration::from_secs(3600)).unwrap();
        let mut store = Store::new(config);
        let mut node = AsyncSync::new(config, 4, Duration::from_millis(10));
        let mut out = Outbox::default();
        // Starts with its share: 25 of 100.
        for id in 0..30 {
            node.on_request(Nanos::ZERO, id, "k", Cost::ONE, &mut out);
        }
        assert_eq!(out.decisions.iter().filter(|(_, d)| d.allowed).count(), 25);
        node.on_tick(Nanos::ZERO, &mut out);
        assert_eq!(out.messages[0].op, StoreOp::Sync { used: 25 });
        deliver(&mut node, &mut store, Nanos::ZERO, &mut out);
        // The store has 75 left, and this node may now use all of it.
        out.clear();
        for id in 0..100 {
            node.on_request(Nanos::ZERO, id, "k", Cost::ONE, &mut out);
        }
        assert_eq!(out.decisions.iter().filter(|(_, d)| d.allowed).count(), 75);
    }

    #[test]
    fn spending_during_a_sync_is_not_counted_twice() {
        let config = RateLimitConfig::new(100, Duration::from_secs(3600)).unwrap();
        let mut store = Store::new(config);
        let mut node = AsyncSync::new(config, 1, Duration::from_millis(10));
        let mut out = Outbox::default();
        node.on_request(Nanos::ZERO, 0, "k", Cost(10), &mut out);
        node.on_tick(Nanos::ZERO, &mut out);
        // 5 more spent while the sync is in flight.
        node.on_request(Nanos::ZERO, 1, "k", Cost(5), &mut out);
        deliver(&mut node, &mut store, Nanos::ZERO, &mut out);
        // Store: 100 - 10 = 90 left; estimate 90 - 5 unsynced = 85.
        out.clear();
        node.on_request(Nanos::ZERO, 2, "k", Cost(85), &mut out);
        node.on_request(Nanos::ZERO, 3, "k", Cost(1), &mut out);
        assert!(out.decisions[0].1.allowed);
        assert!(!out.decisions[1].1.allowed);
    }

    #[test]
    fn a_failed_sync_is_retried_with_the_same_count() {
        let config = RateLimitConfig::per_second(100).unwrap();
        let mut node = AsyncSync::new(config, 2, Duration::from_millis(10));
        let mut out = Outbox::default();
        node.on_request(Nanos::ZERO, 0, "k", Cost(3), &mut out);
        node.on_tick(Nanos::ZERO, &mut out);
        let id = out.messages[0].id;
        out.clear();
        node.on_failure(Nanos::ZERO, id, &mut out);
        node.on_request(Nanos::ZERO, 1, "k", Cost(2), &mut out);
        node.on_tick(Nanos::ZERO, &mut out);
        assert_eq!(out.messages[0].op, StoreOp::Sync { used: 5 });
    }
}
