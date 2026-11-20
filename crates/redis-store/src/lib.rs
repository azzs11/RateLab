//! Redis-backed token bucket, shared by every process that talks to the same Redis.
//!
//! - **One multiplexed connection** per limiter ([`redis::aio::ConnectionManager`]),
//!   reconnecting automatically. It is cheap to share between tasks.
//! - **EVALSHA:** the Lua script (`scripts/token_bucket.lua`) is sent by hash; the full
//!   script is only sent again (`SCRIPT LOAD`) after a `NOSCRIPT` reply.
//! - **Redis `TIME`:** the script reads Redis's clock, so application servers with
//!   skewed clocks still agree on the bucket.
//! - **Exact integers:** token amounts are scaled by the window in microseconds, the
//!   same way `drl-core` scales by nanoseconds, so there is no float rounding.
//! - **Circuit breaker:** [`GuardedLimiter`] wraps the store with a
//!   [`CircuitBreaker`] and applies a [`FailurePolicy`] when Redis cannot answer.
//!
//! ```no_run
//! use drl_core::{BreakerConfig, CircuitBreaker, Cost, FailurePolicy, ManualClock, RateLimitConfig};
//! use drl_redis_store::{GuardedLimiter, RedisRateLimiter};
//! use std::time::Duration;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let config = RateLimitConfig::per_second(100)?;
//! let store = RedisRateLimiter::connect("redis://127.0.0.1/", config).await?;
//!
//! let decision = store.check("user_123", Cost::ONE).await?;
//! println!("allowed: {}", decision.allowed);
//!
//! // With a breaker: if Redis is down, fail closed for this route.
//! let clock = ManualClock::new(); // use drl_core::MonotonicClock in a real server
//! let breaker = CircuitBreaker::new(BreakerConfig::new(5, Duration::from_secs(1))?, clock);
//! let guarded = GuardedLimiter::new(store, breaker);
//! let checked = guarded.check("user_123", Cost::ONE, FailurePolicy::FailClosed).await;
//! println!("allowed: {} (answered by {:?})", checked.decision.allowed, checked.source);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use drl_core::{CircuitBreaker, Clock, Cost, Decision, FailurePolicy, RateLimitConfig, Tokens};
use redis::Script;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use thiserror::Error;

/// The token bucket script, sent to Redis with EVALSHA.
pub const TOKEN_BUCKET_LUA: &str = include_str!("../scripts/token_bucket.lua");

/// Largest integer a Lua (double precision) number holds exactly.
const MAX_EXACT: u128 = 1 << 53;

/// Everything that can go wrong talking to Redis.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Connecting to Redis, a timeout, or running the script failed.
    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),
    /// The script returned something this client did not expect.
    #[error("unexpected reply from rate limit script: {0}")]
    BadReply(String),
    /// The config cannot be computed exactly inside Redis.
    #[error(
        "config not supported by the Redis store: {0} (needs a window of at least 1 µs and limit × window in µs below 2^53)"
    )]
    UnsupportedConfig(String),
}

/// Connection settings.
#[derive(Debug, Clone)]
pub struct RedisOptions {
    /// How long to wait for a reply before the call fails (and counts as a breaker failure).
    pub response_timeout: Duration,
    /// How long to wait when (re)connecting.
    pub connection_timeout: Duration,
    /// Prefix for every key this limiter writes.
    pub key_prefix: String,
}

impl Default for RedisOptions {
    fn default() -> Self {
        Self {
            response_timeout: Duration::from_millis(100),
            connection_timeout: Duration::from_secs(1),
            key_prefix: "drl:tb:".to_owned(),
        }
    }
}

/// Token bucket whose state lives in Redis.
pub struct RedisRateLimiter {
    conn: ConnectionManager,
    script: Script,
    config: RateLimitConfig,
    window_us: u64,
    ttl_ms: u64,
    prefix: String,
    calls: AtomicU64,
}

impl std::fmt::Debug for RedisRateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisRateLimiter")
            .field("config", &self.config)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

/// Window in whole microseconds, checked so the Lua arithmetic stays exact.
fn window_micros(config: RateLimitConfig) -> Result<u64, StoreError> {
    let window_us = config.window_nanos() / 1_000;
    if window_us == 0 {
        return Err(StoreError::UnsupportedConfig(
            "window below 1 µs".to_owned(),
        ));
    }
    // Need fits in 2^53 too: costs above the limit are clamped to limit + 1.
    let largest = (u128::from(config.limit()) + 1) * u128::from(window_us);
    if largest >= MAX_EXACT {
        return Err(StoreError::UnsupportedConfig(format!(
            "limit {} × window {} µs is too large",
            config.limit(),
            window_us
        )));
    }
    Ok(window_us)
}

impl RedisRateLimiter {
    /// Connects to `redis_url` with [`RedisOptions::default`].
    pub async fn connect(redis_url: &str, config: RateLimitConfig) -> Result<Self, StoreError> {
        Self::connect_with(redis_url, config, RedisOptions::default()).await
    }

    /// Connects to `redis_url` with the given options.
    pub async fn connect_with(
        redis_url: &str,
        config: RateLimitConfig,
        options: RedisOptions,
    ) -> Result<Self, StoreError> {
        let window_us = window_micros(config)?;
        let client = redis::Client::open(redis_url)?;
        let manager_config = ConnectionManagerConfig::new()
            .set_response_timeout(Some(options.response_timeout))
            .set_connection_timeout(Some(options.connection_timeout))
            .set_number_of_retries(1);
        let conn = ConnectionManager::new_with_config(client, manager_config).await?;
        Ok(Self {
            conn,
            script: Script::new(TOKEN_BUCKET_LUA),
            config,
            window_us,
            ttl_ms: window_us.div_ceil(1_000) + 1,
            prefix: options.key_prefix,
            calls: AtomicU64::new(0),
        })
    }

    /// The limit this store enforces.
    pub fn config(&self) -> RateLimitConfig {
        self.config
    }

    /// Checks sent to Redis so far. Each is one EVALSHA round trip; after a `NOSCRIPT`
    /// reply the client also sends `SCRIPT LOAD` and retries, which is not counted here
    /// (Redis's own `INFO commandstats` counts every command).
    pub fn redis_calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    /// Checks and charges `cost` units for `key`, atomically in Redis.
    pub async fn check(&self, key: &str, cost: Cost) -> Result<Decision, StoreError> {
        let limit = self.config.limit();
        // A cost above the limit is never admitted; clamping keeps the Lua math exact.
        let sent_cost = cost.0.min(limit + 1);
        let mut conn = self.conn.clone();
        self.calls.fetch_add(1, Ordering::Relaxed);
        let reply: Vec<i64> = self
            .script
            .key(format!("{}{key}", self.prefix))
            .arg(limit)
            .arg(self.window_us)
            .arg(sent_cost)
            .arg(self.ttl_ms)
            .invoke_async(&mut conn)
            .await?;
        let (allowed, level) = match reply.as_slice() {
            [allowed @ (0 | 1), level, _now] if *level >= 0 => (*allowed == 1, *level as u64),
            other => return Err(StoreError::BadReply(format!("{other:?}"))),
        };
        Ok(decide(limit, self.window_us, cost, allowed, level))
    }

    /// Deletes the stored state for `key`.
    pub async fn reset(&self, key: &str) -> Result<(), StoreError> {
        let mut conn = self.conn.clone();
        let _: i64 = redis::cmd("DEL")
            .arg(format!("{}{key}", self.prefix))
            .query_async(&mut conn)
            .await?;
        Ok(())
    }
}

/// Turns the script's reply into a [`Decision`]. `level` is tokens × `window_us`.
fn decide(limit: u64, window_us: u64, cost: Cost, allowed: bool, level: u64) -> Decision {
    let limit_s = u128::from(limit);
    let window = u128::from(window_us);
    let level = u128::from(level);
    let capacity = limit_s * window;
    let micros_until = |scaled: u128| -> Duration {
        let us = scaled.div_ceil(limit_s);
        Duration::from_micros(u64::try_from(us).unwrap_or(u64::MAX))
    };
    let remaining = Tokens(u64::try_from(level / window).unwrap_or(u64::MAX));
    let reset_after = micros_until(capacity.saturating_sub(level));
    if allowed {
        Decision::allow(remaining, reset_after)
    } else {
        let retry_after = (cost.0 <= limit)
            .then(|| micros_until((u128::from(cost.0) * window).saturating_sub(level)));
        Decision::deny(remaining, reset_after, retry_after)
    }
}

/// Who produced a [`Checked`] decision.
#[derive(Debug)]
pub enum Source {
    /// Redis answered.
    Redis,
    /// The breaker was open, so Redis was not called; the failure policy decided.
    BreakerOpen,
    /// Redis was called and failed; the failure policy decided.
    Error(StoreError),
}

/// A decision plus where it came from.
#[derive(Debug)]
pub struct Checked {
    /// The decision to act on.
    pub decision: Decision,
    /// Whether Redis or the failure policy made it.
    pub source: Source,
}

/// A [`RedisRateLimiter`] behind a [`CircuitBreaker`].
///
/// One breaker is shared by all callers; each call passes its own [`FailurePolicy`], so
/// different routes can fail open or closed on the same Redis.
#[derive(Debug)]
pub struct GuardedLimiter<C> {
    store: RedisRateLimiter,
    breaker: CircuitBreaker<C>,
}

impl<C: Clock> GuardedLimiter<C> {
    /// Wraps `store` with `breaker`.
    pub fn new(store: RedisRateLimiter, breaker: CircuitBreaker<C>) -> Self {
        Self { store, breaker }
    }

    /// Checks `key` in Redis unless the breaker is open; otherwise, or if Redis fails,
    /// `policy` decides.
    pub async fn check(&self, key: &str, cost: Cost, policy: FailurePolicy) -> Checked {
        let open_for = self.breaker.config().open_for();
        if let Err(rejected) = self.breaker.acquire() {
            let hint = if rejected.retry_after.is_zero() {
                open_for
            } else {
                rejected.retry_after
            };
            return Checked {
                decision: policy.decide(hint),
                source: Source::BreakerOpen,
            };
        }
        match self.store.check(key, cost).await {
            Ok(decision) => {
                self.breaker.record_success();
                Checked {
                    decision,
                    source: Source::Redis,
                }
            }
            Err(error) => {
                self.breaker.record_failure();
                Checked {
                    decision: policy.decide(open_for),
                    source: Source::Error(error),
                }
            }
        }
    }

    /// The wrapped store.
    pub fn store(&self) -> &RedisRateLimiter {
        &self.store
    }

    /// The breaker.
    pub fn breaker(&self) -> &CircuitBreaker<C> {
        &self.breaker
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_configs_lua_cannot_compute_exactly() {
        let tiny = RateLimitConfig::new(1, Duration::from_nanos(999)).unwrap();
        assert!(matches!(
            window_micros(tiny),
            Err(StoreError::UnsupportedConfig(_))
        ));
        let huge = RateLimitConfig::new(1 << 40, Duration::from_secs(3600)).unwrap();
        assert!(window_micros(huge).is_err());
        let ok = RateLimitConfig::per_second(100_000).unwrap();
        assert_eq!(window_micros(ok).unwrap(), 1_000_000);
    }

    #[test]
    fn decide_matches_the_exact_bucket_math() {
        // 4 per second: one token is 250_000 µs of refill.
        let w = 1_000_000;
        let d = decide(4, w, Cost::ONE, true, 3 * w);
        assert!(d.allowed);
        assert_eq!(d.remaining, Tokens(3));
        assert_eq!(d.reset_after, Duration::from_millis(250));

        // Half a token left: denied, retry when the other half refills.
        let d = decide(4, w, Cost::ONE, false, w / 2);
        assert!(!d.allowed);
        assert_eq!(d.remaining, Tokens(0));
        assert_eq!(d.retry_after, Some(Duration::from_millis(125)));
        assert_eq!(d.reset_after, Duration::from_micros(875_000));

        // Cost above the limit can never succeed.
        let d = decide(4, w, Cost(5), false, 4 * w);
        assert_eq!(d.retry_after, None);
    }

    #[test]
    fn decide_rounds_retry_up_to_the_next_microsecond() {
        // 3 per second: refill is 3 scaled units per µs; 1 missing unit needs 1 µs.
        let w = 1_000_000;
        let d = decide(3, w, Cost::ONE, false, w - 1);
        assert_eq!(d.retry_after, Some(Duration::from_micros(1)));
    }

    #[test]
    fn script_reads_redis_time_not_the_callers() {
        assert!(TOKEN_BUCKET_LUA.contains("redis.call('TIME')"));
        assert!(!TOKEN_BUCKET_LUA.contains("ARGV[5]"));
    }
}
