//! Runs a `ratelab` coordination strategy for real: this node's half of the protocol in
//! memory, the shared store in Redis (`RedisRateLimiter::apply`).
//!
//! The strategy itself does no I/O. This driver gives it the time, sends its messages to
//! Redis, feeds back replies or failures (after `timeout`), runs its periodic ticks, and
//! wakes each waiting request when its decision is made. Most requests are decided
//! immediately; a request waits only when the strategy is fetching from the store.

use drl_redis_store::RedisRateLimiter;
use ratelab::strategies::{CoordinationStrategy, Outbox, RequestId, Strategy};
use ratelab::{Cost, Decision, FailurePolicy, Nanos, RateLimitConfig};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Names accepted by [`StrategyBackend::switch`].
pub const STRATEGIES: [&str; 5] = [
    "central_exact",
    "local_split",
    "async_sync",
    "fixed_leasing",
    "adaptive_leasing",
];

struct Node {
    name: String,
    strategy: Box<dyn CoordinationStrategy>,
    waiters: HashMap<RequestId, oneshot::Sender<Decision>>,
    next_id: RequestId,
    /// Bumped when the strategy is replaced, so late replies to the old one are ignored.
    generation: u64,
}

/// One node of a cluster running a coordination strategy against Redis.
pub struct StrategyBackend {
    store: Arc<RedisRateLimiter>,
    config: RateLimitConfig,
    nodes: u64,
    policy: FailurePolicy,
    timeout: Duration,
    origin: Instant,
    node: Mutex<Node>,
}

fn build(
    name: &str,
    config: RateLimitConfig,
    nodes: u64,
    policy: FailurePolicy,
) -> Option<Box<dyn CoordinationStrategy>> {
    Strategy::defaults(config, nodes)
        .into_iter()
        .find(|s| s.name() == name)
        .map(|s| s.build(config, nodes, policy))
}

impl StrategyBackend {
    /// A node running `name` in a cluster of `nodes`. Starts the tick task.
    pub fn start(
        name: &str,
        store: RedisRateLimiter,
        nodes: u64,
        policy: FailurePolicy,
        timeout: Duration,
    ) -> Result<Arc<Self>, String> {
        let config = store.config();
        let strategy = build(name, config, nodes, policy)
            .ok_or_else(|| format!("unknown strategy {name:?}"))?;
        let this = Arc::new(Self {
            store: Arc::new(store),
            config,
            nodes,
            policy,
            timeout,
            origin: Instant::now(),
            node: Mutex::new(Node {
                name: name.to_owned(),
                strategy,
                waiters: HashMap::new(),
                next_id: 0,
                generation: 0,
            }),
        });
        let ticker = Arc::clone(&this);
        tokio::spawn(async move { ticker.tick_loop().await });
        Ok(this)
    }

    fn now(&self) -> Nanos {
        Nanos(u64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(u64::MAX))
    }

    fn lock(&self) -> MutexGuard<'_, Node> {
        self.node.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The strategy now running.
    pub fn name(&self) -> String {
        self.lock().name.clone()
    }

    /// The limit, for headers.
    pub fn config(&self) -> RateLimitConfig {
        self.config
    }

    /// Store operations sent so far.
    pub fn store_calls(&self) -> u64 {
        self.store.redis_calls()
    }

    /// Replaces the running strategy (benchmarks only). Waiting requests are decided by
    /// the failure policy; late replies to the old strategy are ignored.
    pub fn switch(&self, name: &str) -> Result<(), String> {
        let strategy = build(name, self.config, self.nodes, self.policy)
            .ok_or_else(|| format!("unknown strategy {name:?}"))?;
        let mut node = self.lock();
        node.strategy = strategy;
        node.name = name.to_owned();
        node.generation += 1;
        let policy = self.policy;
        for (_, tx) in node.waiters.drain() {
            let _ = tx.send(policy.decide(Duration::ZERO));
        }
        Ok(())
    }

    /// Decides one request; waits only if the strategy needs the store first.
    pub async fn check(self: &Arc<Self>, key: &str, cost: Cost) -> Decision {
        let (tx, rx) = oneshot::channel();
        let mut out = Outbox::default();
        let generation = {
            let now = self.now();
            let mut node = self.lock();
            let id = node.next_id;
            node.next_id += 1;
            node.waiters.insert(id, tx);
            node.strategy.on_request(now, id, key, cost, &mut out);
            node.generation
        };
        self.dispatch(out, generation);
        // A decision always comes (reply, failure or policy); the bound only guards bugs.
        match tokio::time::timeout(self.timeout * 4, rx).await {
            Ok(Ok(decision)) => decision,
            _ => self.policy.decide(self.timeout),
        }
    }

    /// Delivers decisions and sends messages from an outbox.
    fn dispatch(self: &Arc<Self>, out: Outbox, generation: u64) {
        if !out.decisions.is_empty() {
            let mut node = self.lock();
            for (id, decision) in out.decisions {
                if let Some(tx) = node.waiters.remove(&id) {
                    let _ = tx.send(decision);
                }
            }
        }
        for msg in out.messages {
            let this = Arc::clone(self);
            tokio::spawn(async move {
                let result =
                    tokio::time::timeout(this.timeout, this.store.apply(&msg.key, &msg.op)).await;
                let mut next = Outbox::default();
                {
                    let now = this.now();
                    let mut node = this.lock();
                    if node.generation != generation {
                        return;
                    }
                    match result {
                        Ok(Ok(reply)) => node.strategy.on_reply(now, msg.id, reply, &mut next),
                        Ok(Err(e)) => {
                            tracing::debug!(error = %e, "store call failed");
                            node.strategy.on_failure(now, msg.id, &mut next);
                        }
                        Err(_) => node.strategy.on_failure(now, msg.id, &mut next),
                    }
                }
                this.dispatch(next, generation);
            });
        }
    }

    async fn tick_loop(self: Arc<Self>) {
        loop {
            let every = self.lock().strategy.tick_every();
            tokio::time::sleep(every.unwrap_or(Duration::from_millis(250))).await;
            if every.is_none() {
                continue;
            }
            let mut out = Outbox::default();
            let generation = {
                let now = self.now();
                let mut node = self.lock();
                node.strategy.on_tick(now, &mut out);
                node.generation
            };
            self.dispatch(out, generation);
        }
    }
}
