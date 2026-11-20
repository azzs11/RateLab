//! Quota leasing: nodes borrow tokens from the store and spend them locally.

use super::{CoordinationStrategy, Message, MessageId, Outbox, RequestId, StoreOp, StoreReply};
use crate::breaker::FailurePolicy;
use crate::{Cost, Decision, Nanos, RateLimitConfig, Tokens};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::Duration;

/// How an [`AdaptiveLeasing`] node sizes its leases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveParams {
    /// First lease size.
    pub start: u64,
    /// Smallest lease.
    pub min: u64,
    /// Largest lease.
    pub max: u64,
    /// A lease used up faster than this doubles the next one.
    pub target_lifetime: Duration,
    /// Leftovers are returned (and the size halved) after this long without requests.
    pub idle_return: Duration,
}

impl AdaptiveParams {
    /// Defaults for a `nodes`-node cluster: start at `limit / 8N`, between 1 and
    /// `limit / 2`, target lifetime a tenth of the window, return after a fifth of the
    /// window idle.
    ///
    /// ```
    /// use drl_core::strategies::AdaptiveParams;
    /// use drl_core::RateLimitConfig;
    ///
    /// let p = AdaptiveParams::for_cluster(RateLimitConfig::per_second(100)?, 3);
    /// assert_eq!((p.start, p.min, p.max), (4, 1, 50));
    /// # Ok::<(), drl_core::ConfigError>(())
    /// ```
    pub fn for_cluster(config: RateLimitConfig, nodes: u64) -> Self {
        let limit = config.limit();
        Self {
            start: (limit / (8 * nodes.max(1))).max(1),
            min: 1,
            max: (limit / 2).max(1),
            target_lifetime: config.window() / 10,
            idle_return: config.window() / 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sizing {
    Fixed { chunk: u64, ttl: Duration },
    Adaptive(AdaptiveParams),
}

#[derive(Debug)]
struct KeyLease {
    tokens: u64,
    /// Fixed leasing: leftovers are lost at this time.
    expires: Nanos,
    granted_at: Option<Nanos>,
    size: u64,
    pending: Option<(MessageId, u64)>,
    queue: VecDeque<(RequestId, Cost)>,
    queued_cost: u64,
    /// After the store said "empty" or failed: no new lease request until then.
    backoff_until: Nanos,
    /// Whether the backoff came from a failure (apply the policy) or an empty store (deny).
    backoff_failure: bool,
    retry_hint: Duration,
    last_request: Nanos,
}

#[derive(Debug)]
struct Leasing {
    limit: u64,
    sizing: Sizing,
    policy: FailurePolicy,
    failure_backoff: Duration,
    /// Longest wait after the store had nothing to lease.
    max_backoff: Duration,
    keys: BTreeMap<String, KeyLease>,
    next: MessageId,
    pending: HashMap<MessageId, String>,
}

fn allow(tokens: u64) -> Decision {
    Decision::allow(Tokens(tokens), Duration::ZERO)
}

impl Leasing {
    fn new(config: RateLimitConfig, sizing: Sizing, policy: FailurePolicy) -> Self {
        Self {
            limit: config.limit(),
            sizing,
            policy,
            failure_backoff: config.window(),
            max_backoff: config.window() / 10,
            keys: BTreeMap::new(),
            next: 0,
            pending: HashMap::new(),
        }
    }

    fn start_size(&self) -> u64 {
        match self.sizing {
            Sizing::Fixed { chunk, .. } => chunk.max(1),
            Sizing::Adaptive(p) => p.start.clamp(p.min.max(1), p.max.max(1)),
        }
    }

    /// Sends a lease request for `key` covering what is queued.
    fn request_lease(&mut self, now: Nanos, key: &str, out: &mut Outbox) {
        let sizing = self.sizing;
        let msg = self.next;
        let Some(lease) = self.keys.get_mut(key) else {
            return;
        };
        if let Sizing::Adaptive(p) = sizing
            && lease
                .granted_at
                .is_some_and(|at| now.saturating_since(at) < p.target_lifetime)
        {
            // Used up quickly: borrow more next time.
            lease.size = lease.size.saturating_mul(2).min(p.max.max(1));
        }
        let want = lease
            .size
            .max(lease.queued_cost.saturating_sub(lease.tokens));
        lease.pending = Some((msg, want));
        self.next += 1;
        self.pending.insert(msg, key.to_owned());
        out.messages.push(Message {
            id: msg,
            key: key.to_owned(),
            op: StoreOp::Lease { want },
        });
    }

    /// Admits queued requests the lease covers; returns whether any are left.
    fn drain(lease: &mut KeyLease, out: &mut Outbox) -> bool {
        while let Some(&(id, cost)) = lease.queue.front() {
            if cost.0 > lease.tokens {
                break;
            }
            lease.tokens -= cost.0;
            lease.queued_cost -= cost.0;
            lease.queue.pop_front();
            out.decisions.push((id, allow(lease.tokens)));
        }
        !lease.queue.is_empty()
    }

    fn reject_queue(lease: &mut KeyLease, decision: Decision, out: &mut Outbox) {
        for (id, _) in lease.queue.drain(..) {
            out.decisions.push((id, decision));
        }
        lease.queued_cost = 0;
    }

    fn on_request(&mut self, now: Nanos, id: RequestId, key: &str, cost: Cost, out: &mut Outbox) {
        if cost.0 > self.limit {
            out.decisions
                .push((id, Decision::deny(Tokens(0), Duration::ZERO, None)));
            return;
        }
        let start = self.start_size();
        let lease = self.keys.entry(key.to_owned()).or_insert_with(|| KeyLease {
            tokens: 0,
            expires: Nanos::ZERO,
            granted_at: None,
            size: start,
            pending: None,
            queue: VecDeque::new(),
            queued_cost: 0,
            backoff_until: Nanos::ZERO,
            backoff_failure: false,
            retry_hint: Duration::ZERO,
            last_request: now,
        });
        lease.last_request = now;
        if let Sizing::Fixed { .. } = self.sizing
            && lease.tokens > 0
            && now >= lease.expires
        {
            lease.tokens = 0; // fixed leases expire; leftovers are lost
        }
        if lease.pending.is_some() {
            lease.queue.push_back((id, cost));
            lease.queued_cost += cost.0;
            return;
        }
        if lease.tokens >= cost.0 {
            lease.tokens -= cost.0;
            out.decisions.push((id, allow(lease.tokens)));
            return;
        }
        if now < lease.backoff_until {
            let decision = if lease.backoff_failure {
                self.policy
                    .decide(lease.backoff_until.saturating_since(now))
            } else {
                Decision::deny(
                    Tokens(lease.tokens),
                    lease.retry_hint,
                    Some(lease.backoff_until.saturating_since(now)),
                )
            };
            out.decisions.push((id, decision));
            return;
        }
        lease.queue.push_back((id, cost));
        lease.queued_cost += cost.0;
        self.request_lease(now, key, out);
    }

    fn on_reply(&mut self, now: Nanos, id: MessageId, reply: StoreReply, out: &mut Outbox) {
        let Some(key) = self.pending.remove(&id) else {
            return;
        };
        let StoreReply::Leased {
            granted,
            retry_after,
        } = reply
        else {
            return; // a Return's acknowledgement
        };
        let sizing = self.sizing;
        let max_backoff = self.max_backoff;
        let Some(lease) = self.keys.get_mut(&key) else {
            return;
        };
        let want = lease.pending.take().map_or(0, |(_, want)| want);
        lease.tokens += granted;
        lease.granted_at = Some(now);
        match sizing {
            Sizing::Fixed { ttl, .. } => lease.expires = now.saturating_add(ttl),
            Sizing::Adaptive(p) if granted < want => {
                // The store is running low: hold less.
                lease.size = (lease.size / 2).max(p.min.max(1));
            }
            Sizing::Adaptive(_) => {}
        }
        let waiting = Self::drain(lease, out);
        if !waiting {
            return;
        }
        if granted == 0 {
            // The store says when the whole lease will be there; do not wait longer than
            // a tenth of the window, or a node with a large lease stalls in bursts.
            let retry = retry_after.unwrap_or(Duration::ZERO).min(max_backoff);
            lease.backoff_until = now.saturating_add(retry);
            lease.backoff_failure = false;
            lease.retry_hint = retry;
            Self::reject_queue(lease, Decision::deny(Tokens(0), retry, Some(retry)), out);
        } else {
            self.request_lease(now, &key, out);
        }
    }

    fn on_failure(&mut self, now: Nanos, id: MessageId, out: &mut Outbox) {
        let Some(key) = self.pending.remove(&id) else {
            return;
        };
        let (policy, backoff) = (self.policy, self.failure_backoff);
        if let Some(lease) = self.keys.get_mut(&key)
            && lease.pending.is_some_and(|(m, _)| m == id)
        {
            lease.pending = None;
            lease.backoff_until = now.saturating_add(backoff);
            lease.backoff_failure = true;
            Self::reject_queue(lease, policy.decide(backoff), out);
        }
    }

    fn on_tick(&mut self, now: Nanos, out: &mut Outbox) {
        let Sizing::Adaptive(p) = self.sizing else {
            return;
        };
        for (key, lease) in &mut self.keys {
            if lease.tokens == 0
                || lease.pending.is_some()
                || now.saturating_since(lease.last_request) < p.idle_return
            {
                continue;
            }
            let msg = self.next;
            self.next += 1;
            self.pending.insert(msg, key.clone());
            out.messages.push(Message {
                id: msg,
                key: key.clone(),
                op: StoreOp::Return {
                    amount: lease.tokens,
                },
            });
            lease.tokens = 0;
            lease.size = (lease.size / 2).max(p.min.max(1));
        }
    }
}

/// Borrows a fixed `chunk` of tokens from the store whenever its lease runs out and
/// spends them locally. Unused tokens expire after `ttl` and are lost, which
/// under-admits when traffic moves between nodes. Requests that arrive while a lease is
/// being fetched wait for it.
///
/// ```
/// use drl_core::strategies::{CoordinationStrategy, FixedLeasing, Outbox, Store};
/// use drl_core::{Cost, FailurePolicy, Nanos, RateLimitConfig};
/// use std::time::Duration;
///
/// let config = RateLimitConfig::per_second(100)?;
/// let mut store = Store::new(config);
/// let mut node = FixedLeasing::new(config, 10, Duration::from_secs(1), FailurePolicy::FailOpen);
/// let mut out = Outbox::default();
///
/// node.on_request(Nanos::ZERO, 1, "k", Cost::ONE, &mut out); // waits for a lease
/// let m = out.messages.pop().unwrap();
/// let reply = store.handle(Nanos::ZERO, &m.key, &m.op);
/// node.on_reply(Nanos::ZERO, m.id, reply, &mut out);
/// assert!(out.decisions[0].1.allowed);
///
/// // The next 9 are served from the lease, with no store traffic.
/// for id in 2..11 {
///     node.on_request(Nanos::ZERO, id, "k", Cost::ONE, &mut out);
/// }
/// assert!(out.messages.is_empty());
/// assert_eq!(store.tokens(Nanos::ZERO, "k"), 90);
/// # Ok::<(), drl_core::ConfigError>(())
/// ```
#[derive(Debug)]
pub struct FixedLeasing(Leasing);

impl FixedLeasing {
    /// Leases of `chunk` tokens that expire after `ttl`.
    pub fn new(config: RateLimitConfig, chunk: u64, ttl: Duration, policy: FailurePolicy) -> Self {
        Self(Leasing::new(config, Sizing::Fixed { chunk, ttl }, policy))
    }
}

/// Leasing whose lease size follows the node's demand, like a TCP window: it doubles
/// when a lease is used up quickly, halves when the store grants less than asked, and
/// when the node is idle it returns its leftovers to the store (and halves). Busy nodes
/// borrow more; idle nodes do not hoard.
///
/// ```
/// use drl_core::strategies::{AdaptiveLeasing, AdaptiveParams, CoordinationStrategy, Outbox, Store};
/// use drl_core::{Cost, FailurePolicy, Nanos, RateLimitConfig};
///
/// let config = RateLimitConfig::per_second(100)?;
/// let mut store = Store::new(config);
/// let params = AdaptiveParams::for_cluster(config, 1);
/// let mut node = AdaptiveLeasing::new(config, params, FailurePolicy::FailOpen);
/// let mut out = Outbox::default();
///
/// node.on_request(Nanos::ZERO, 1, "k", Cost::ONE, &mut out);
/// let m = out.messages.pop().unwrap();
/// let reply = store.handle(Nanos::ZERO, &m.key, &m.op);
/// node.on_reply(Nanos::ZERO, m.id, reply, &mut out);
/// assert!(out.decisions[0].1.allowed);
///
/// // Idle for longer than `idle_return`: the leftovers go back to the store.
/// node.on_tick(Nanos(500_000_000), &mut out);
/// let m = out.messages.pop().unwrap();
/// store.handle(Nanos(500_000_000), &m.key, &m.op);
/// assert_eq!(store.tokens(Nanos(500_000_000), "k"), 100);
/// # Ok::<(), drl_core::ConfigError>(())
/// ```
#[derive(Debug)]
pub struct AdaptiveLeasing(Leasing);

impl AdaptiveLeasing {
    /// Adaptive leases sized by `params`.
    pub fn new(config: RateLimitConfig, params: AdaptiveParams, policy: FailurePolicy) -> Self {
        Self(Leasing::new(config, Sizing::Adaptive(params), policy))
    }
}

macro_rules! delegate {
    ($t:ty, $name:literal, $tick:expr) => {
        impl CoordinationStrategy for $t {
            fn name(&self) -> &'static str {
                $name
            }
            fn on_request(
                &mut self,
                now: Nanos,
                id: RequestId,
                key: &str,
                cost: Cost,
                out: &mut Outbox,
            ) {
                self.0.on_request(now, id, key, cost, out);
            }
            fn on_reply(&mut self, now: Nanos, id: MessageId, reply: StoreReply, out: &mut Outbox) {
                self.0.on_reply(now, id, reply, out);
            }
            fn on_failure(&mut self, now: Nanos, id: MessageId, out: &mut Outbox) {
                self.0.on_failure(now, id, out);
            }
            fn tick_every(&self) -> Option<Duration> {
                let tick: fn(&Leasing) -> Option<Duration> = $tick;
                tick(&self.0)
            }
            fn on_tick(&mut self, now: Nanos, out: &mut Outbox) {
                self.0.on_tick(now, out);
            }
        }
    };
}

delegate!(FixedLeasing, "fixed_leasing", |_| None);
delegate!(AdaptiveLeasing, "adaptive_leasing", |l| match l.sizing {
    Sizing::Adaptive(p) => Some((p.idle_return / 2).max(Duration::from_nanos(1))),
    Sizing::Fixed { .. } => None,
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategies::Store;

    /// Delivers every outgoing message to the store at `now` until none are left.
    fn settle(
        node: &mut dyn CoordinationStrategy,
        store: &mut Store,
        now: Nanos,
        out: &mut Outbox,
    ) {
        while let Some(m) = out.messages.pop() {
            let reply = store.handle(now, &m.key, &m.op);
            node.on_reply(now, m.id, reply, out);
        }
    }

    fn config() -> RateLimitConfig {
        RateLimitConfig::new(100, Duration::from_secs(1)).unwrap()
    }

    #[test]
    fn requests_wait_for_the_lease_in_flight() {
        let mut store = Store::new(config());
        let mut node =
            FixedLeasing::new(config(), 5, Duration::from_secs(1), FailurePolicy::FailOpen);
        let mut out = Outbox::default();
        for id in 0..3 {
            node.on_request(Nanos::ZERO, id, "k", Cost::ONE, &mut out);
        }
        assert_eq!(out.messages.len(), 1); // one lease for all three
        assert!(out.decisions.is_empty());
        settle(&mut node, &mut store, Nanos::ZERO, &mut out);
        assert_eq!(out.decisions.len(), 3);
        assert!(out.decisions.iter().all(|(_, d)| d.allowed));
    }

    #[test]
    fn fixed_leftovers_expire() {
        let mut store = Store::new(config());
        let mut node = FixedLeasing::new(
            config(),
            10,
            Duration::from_millis(100),
            FailurePolicy::FailOpen,
        );
        let mut out = Outbox::default();
        node.on_request(Nanos::ZERO, 0, "k", Cost::ONE, &mut out);
        settle(&mut node, &mut store, Nanos::ZERO, &mut out);
        out.clear();
        // After the ttl the 9 leftovers are gone, so a new lease is needed.
        node.on_request(Nanos(100_000_000), 1, "k", Cost::ONE, &mut out);
        assert_eq!(out.messages.len(), 1);
    }

    #[test]
    fn an_empty_store_denies_and_backs_off() {
        let config = RateLimitConfig::new(2, Duration::from_secs(1)).unwrap();
        let mut store = Store::new(config);
        let mut node =
            FixedLeasing::new(config, 2, Duration::from_secs(10), FailurePolicy::FailOpen);
        let mut out = Outbox::default();
        for id in 0..3 {
            node.on_request(Nanos::ZERO, id, "k", Cost::ONE, &mut out);
            settle(&mut node, &mut store, Nanos::ZERO, &mut out);
        }
        let denied = out.decisions[2].1;
        assert!(!denied.allowed);
        // The store says a whole lease of 2 is back in 1 s; the node waits at most a
        // tenth of the window.
        assert_eq!(denied.retry_after, Some(Duration::from_millis(100)));
        out.clear();
        // Within the backoff: denied locally, no store traffic.
        node.on_request(Nanos(100), 3, "k", Cost::ONE, &mut out);
        assert!(out.messages.is_empty());
        assert!(!out.decisions[0].1.allowed);
    }

    #[test]
    fn adaptive_grows_when_used_up_quickly_and_shrinks_when_scarce() {
        let mut store = Store::new(config());
        let p = AdaptiveParams::for_cluster(config(), 4); // start 3, max 50
        let mut node = AdaptiveLeasing::new(config(), p, FailurePolicy::FailOpen);
        let mut out = Outbox::default();
        let mut wants = Vec::new();
        for id in 0..40 {
            node.on_request(Nanos(id * 1_000), id, "k", Cost::ONE, &mut out);
            for m in &out.messages {
                if let StoreOp::Lease { want } = m.op {
                    wants.push(want);
                }
            }
            settle(&mut node, &mut store, Nanos(id * 1_000), &mut out);
        }
        assert_eq!(wants, [3, 6, 12, 24]);
        assert!(out.decisions.iter().all(|(_, d)| d.allowed));
    }

    #[test]
    fn failure_applies_the_policy_after_spending_what_is_held() {
        for (policy, allowed) in [
            (FailurePolicy::FailOpen, true),
            (FailurePolicy::FailClosed, false),
        ] {
            let mut node = FixedLeasing::new(config(), 5, Duration::from_secs(1), policy);
            let mut out = Outbox::default();
            node.on_request(Nanos::ZERO, 0, "k", Cost::ONE, &mut out);
            let id = out.messages[0].id;
            node.on_failure(Nanos::ZERO, id, &mut out);
            assert_eq!(out.decisions[0].1.allowed, allowed);
            out.clear();
            // During the backoff the policy decides without new store traffic.
            node.on_request(Nanos(1), 1, "k", Cost::ONE, &mut out);
            assert!(out.messages.is_empty());
            assert_eq!(out.decisions[0].1.allowed, allowed);
        }
    }

    #[test]
    fn costs_above_the_limit_are_denied_without_store_traffic() {
        let mut node = AdaptiveLeasing::new(
            config(),
            AdaptiveParams::for_cluster(config(), 1),
            FailurePolicy::FailOpen,
        );
        let mut out = Outbox::default();
        node.on_request(Nanos::ZERO, 0, "k", Cost(101), &mut out);
        assert!(out.messages.is_empty());
        assert_eq!(out.decisions[0].1.retry_after, None);
    }
}
