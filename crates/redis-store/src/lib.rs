//! Redis-backed token bucket **prototype**.
//!
//! The bucket lives in Redis and is updated by one atomic Lua script, so several
//! processes can share a limit. Known gaps, fixed in Phase 2 of the roadmap:
//!
//! - a new Redis connection is opened for every check (no pool);
//! - time comes from this process's clock, not from Redis `TIME`;
//! - [`RedisRateLimiter::check_with_fallback`] simply fails open; there is no circuit
//!   breaker yet.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use drl_core::{Cost, Decision, RateLimitConfig, Tokens};
use redis::{Client, Commands, Script};
use std::time::{Duration, SystemTime};
use thiserror::Error;

/// Everything that can go wrong talking to Redis.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Connecting to Redis or running the script failed.
    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),
    /// The script returned something this client did not expect.
    #[error("unexpected reply from rate limit script: {0}")]
    BadReply(String),
}

/// Atomic token bucket check. Returns {allowed, tokens_after_as_string}.
const TOKEN_BUCKET_LUA: &str = r#"
local key = KEYS[1]
local max_tokens = tonumber(ARGV[1])
local refill_rate = tonumber(ARGV[2])
local now = tonumber(ARGV[3])
local cost = tonumber(ARGV[5])

local state = redis.call('HMGET', key, 'tokens', 'last_refill')
local tokens = tonumber(state[1]) or max_tokens
local last_refill = tonumber(state[2]) or now

local elapsed = math.max(0, now - last_refill)
tokens = math.min(tokens + elapsed * refill_rate, max_tokens)

local allowed = 0
if tokens >= cost then
    tokens = tokens - cost
    allowed = 1
end

redis.call('HSET', key, 'tokens', tokens, 'last_refill', now)
redis.call('EXPIRE', key, ARGV[4])

return {allowed, tostring(tokens)}
"#;

/// Token bucket whose state is shared through Redis.
pub struct RedisRateLimiter {
    client: Client,
    config: RateLimitConfig,
    script: Script,
}

impl RedisRateLimiter {
    /// Creates a limiter for the Redis server at `redis_url`. Does not connect yet.
    pub fn new(redis_url: &str, config: RateLimitConfig) -> Result<Self, StoreError> {
        Ok(Self {
            client: Client::open(redis_url)?,
            config,
            script: Script::new(TOKEN_BUCKET_LUA),
        })
    }

    /// Checks and charges `cost` units for `key`.
    pub fn check(&self, key: &str, cost: Cost) -> Result<Decision, StoreError> {
        let mut conn = self.client.get_connection()?;

        let limit = self.config.limit() as f64;
        let window_secs = self.config.window().as_secs_f64();
        let refill_rate = limit / window_secs;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let ttl_secs = (window_secs * 2.0).ceil().max(1.0) as u64;

        let (allowed, tokens): (i64, String) = self
            .script
            .key(format!("rate_limit:{key}"))
            .arg(limit)
            .arg(refill_rate)
            .arg(now)
            .arg(ttl_secs)
            .arg(cost.0)
            .invoke(&mut conn)?;
        let tokens: f64 = tokens
            .parse()
            .map_err(|_| StoreError::BadReply(tokens.clone()))?;

        let remaining = Tokens(tokens.max(0.0).floor() as u64);
        let reset_after = secs((limit - tokens) / refill_rate);
        Ok(if allowed == 1 {
            Decision::allow(remaining, reset_after)
        } else {
            let retry_after = (cost.0 <= self.config.limit())
                .then(|| secs((cost.0 as f64 - tokens) / refill_rate));
            Decision::deny(remaining, reset_after, retry_after)
        })
    }

    /// Like [`check`](Self::check), but if Redis cannot be reached the request is
    /// **allowed** (fail-open) and the error is logged to stderr. This is a plain
    /// fallback, not a circuit breaker.
    pub fn check_with_fallback(&self, key: &str, cost: Cost) -> Decision {
        self.check(key, cost).unwrap_or_else(|e| {
            eprintln!("rate limiter: Redis unavailable, allowing request (fail-open): {e}");
            Decision::allow(Tokens(0), Duration::ZERO)
        })
    }

    /// Deletes the stored state for `key`.
    pub fn reset(&self, key: &str) -> Result<(), StoreError> {
        let mut conn = self.client.get_connection()?;
        let _: i64 = conn.del(format!("rate_limit:{key}"))?;
        Ok(())
    }
}

/// Non-negative seconds as a `Duration`, rounded up to the next nanosecond.
fn secs(s: f64) -> Duration {
    Duration::from_nanos((s.max(0.0) * 1e9).ceil() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_url_is_an_error() {
        let config = RateLimitConfig::per_second(5).unwrap();
        assert!(RedisRateLimiter::new("not a url", config).is_err());
    }

    #[test]
    fn secs_rounds_up_and_clamps() {
        assert_eq!(secs(-1.0), Duration::ZERO);
        assert_eq!(secs(0.5), Duration::from_millis(500));
    }

    /// Needs a Redis server on 127.0.0.1:6379. Phase 2 replaces this with
    /// testcontainers so it runs in CI instead of being skipped.
    #[test]
    #[ignore = "needs a running Redis on 127.0.0.1:6379"]
    fn allows_limit_then_denies() {
        let config = RateLimitConfig::per_second(5).unwrap();
        let limiter = RedisRateLimiter::new("redis://127.0.0.1/", config).unwrap();
        limiter.reset("test_user").unwrap();
        for _ in 0..5 {
            assert!(limiter.check("test_user", Cost::ONE).unwrap().allowed);
        }
        let denied = limiter.check("test_user", Cost::ONE).unwrap();
        assert!(!denied.allowed);
        assert!(denied.retry_after.is_some());
        limiter.reset("test_user").unwrap();
    }
}
