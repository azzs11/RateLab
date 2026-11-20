//! Where decisions come from: an in-process limiter or Redis behind a circuit breaker.

use drl_core::{
    Clock, Cost, Decision, FailurePolicy, MonotonicClock, RateLimitConfig, RateLimiter,
};
use drl_redis_store::{GuardedLimiter, Source};

/// Who made a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionSource {
    /// An in-process limiter (state is per node).
    Local,
    /// Redis (state is shared by every node).
    Redis,
    /// The breaker was open; the route's failure policy decided.
    BreakerOpen,
    /// Redis failed; the route's failure policy decided.
    RedisError,
}

impl DecisionSource {
    /// Lower-case name for headers, logs and metrics.
    pub fn name(self) -> &'static str {
        match self {
            DecisionSource::Local => "local",
            DecisionSource::Redis => "redis",
            DecisionSource::BreakerOpen => "breaker_open",
            DecisionSource::RedisError => "redis_error",
        }
    }

    /// Whether the limiter itself decided (as opposed to a failure policy).
    pub fn is_enforced(self) -> bool {
        matches!(self, DecisionSource::Local | DecisionSource::Redis)
    }
}

/// A decision and its source.
#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    /// What to do with the request.
    pub decision: Decision,
    /// Who decided.
    pub source: DecisionSource,
}

/// The rate limit backend shared by every limited route.
pub enum Backend<C = MonotonicClock> {
    /// One process, in-memory state.
    Local {
        /// The limiter.
        limiter: Box<dyn RateLimiter>,
        /// Its limit, for the `RateLimit-Policy` header.
        config: RateLimitConfig,
    },
    /// Redis-backed token bucket shared by all nodes.
    Redis(GuardedLimiter<C>),
}

impl<C: Clock> Backend<C> {
    /// The limit every route enforces.
    pub fn config(&self) -> RateLimitConfig {
        match self {
            Backend::Local { config, .. } => *config,
            Backend::Redis(guarded) => guarded.store().config(),
        }
    }

    /// Short name: `local` or `redis`.
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Local { .. } => "local",
            Backend::Redis(_) => "redis",
        }
    }

    /// Checks and charges `cost` for `key`; `policy` applies if Redis cannot answer.
    pub async fn check(&self, key: &str, cost: Cost, policy: FailurePolicy) -> Outcome {
        match self {
            Backend::Local { limiter, .. } => Outcome {
                decision: limiter.check(key, cost),
                source: DecisionSource::Local,
            },
            Backend::Redis(guarded) => {
                let checked = guarded.check(key, cost, policy).await;
                let source = match checked.source {
                    Source::Redis => DecisionSource::Redis,
                    Source::BreakerOpen => DecisionSource::BreakerOpen,
                    Source::Error(e) => {
                        tracing::debug!(error = %e, policy = policy.name(), "Redis check failed");
                        DecisionSource::RedisError
                    }
                };
                Outcome {
                    decision: checked.decision,
                    source,
                }
            }
        }
    }
}
