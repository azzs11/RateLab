//! Integration tests against a real Redis, started by testcontainers.
//!
//! They need Docker, so they are `#[ignore]` for a plain `cargo test`. CI runs them
//! with `cargo test -p drl-redis-store -- --ignored` on every push, so they are never
//! skipped there. Locally: start Docker, then run the same command.

use drl_redis_store::{GuardedLimiter, RedisOptions, RedisRateLimiter, Source};
use ratelab::strategies::{Store, StoreOp, StoreReply};
use ratelab::{
    BreakerConfig, BreakerState, CircuitBreaker, Cost, FailurePolicy, ManualClock, Nanos,
};
use ratelab::{RateLimitConfig, Tokens};
use std::sync::Arc;
use std::time::Duration;
use testcontainers_modules::redis::{REDIS_PORT, Redis};
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

async fn redis() -> (ContainerAsync<Redis>, String) {
    let container = Redis::default()
        .with_tag("7.4-alpine")
        .start()
        .await
        .expect("start Redis container (is Docker running?)");
    let host = container.get_host().await.expect("container host");
    let port = container
        .get_host_port_ipv4(REDIS_PORT)
        .await
        .expect("container port");
    (container, format!("redis://{host}:{port}/"))
}

async fn raw(url: &str) -> redis::aio::MultiplexedConnection {
    redis::Client::open(url)
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap()
}

/// A limit that refills slowly enough that no token comes back during a test.
fn slow(limit: u64) -> RateLimitConfig {
    RateLimitConfig::new(limit, Duration::from_secs(3600)).unwrap()
}

#[tokio::test]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn allows_the_limit_then_denies_with_a_retry_hint() {
    let (_c, url) = redis().await;
    let store = RedisRateLimiter::connect(&url, slow(5)).await.unwrap();
    for left in (0..5).rev() {
        let d = store.check("alice", Cost::ONE).await.unwrap();
        assert!(d.allowed);
        assert_eq!(d.remaining, Tokens(left));
    }
    let denied = store.check("alice", Cost::ONE).await.unwrap();
    assert!(!denied.allowed);
    // One token is 3600 s / 5 = 720 s of refill; a little has refilled already.
    let retry = denied.retry_after.unwrap();
    assert!(retry <= Duration::from_secs(720) && retry > Duration::from_secs(719));
    assert_eq!(store.redis_calls(), 6);
}

#[tokio::test]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn separate_clients_share_one_limit() {
    let (_c, url) = redis().await;
    let a = RedisRateLimiter::connect(&url, slow(10)).await.unwrap();
    let b = RedisRateLimiter::connect(&url, slow(10)).await.unwrap();
    let mut admitted = 0;
    for i in 0..40 {
        let node = if i % 2 == 0 { &a } else { &b };
        if node.check("shared", Cost::ONE).await.unwrap().allowed {
            admitted += 1;
        }
    }
    assert_eq!(admitted, 10);
}

#[tokio::test]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn concurrent_checks_never_over_admit() {
    let (_c, url) = redis().await;
    let store = Arc::new(RedisRateLimiter::connect(&url, slow(100)).await.unwrap());
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let store = Arc::clone(&store);
            tokio::spawn(async move {
                let mut admitted = 0u32;
                for _ in 0..50 {
                    if store.check("hot", Cost::ONE).await.unwrap().allowed {
                        admitted += 1;
                    }
                }
                admitted
            })
        })
        .collect();
    let mut total = 0;
    for t in tasks {
        total += t.await.unwrap();
    }
    assert_eq!(total, 100);
}

#[tokio::test]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn cost_is_charged_and_oversized_costs_are_never_admitted() {
    let (_c, url) = redis().await;
    let store = RedisRateLimiter::connect(&url, slow(10)).await.unwrap();
    let d = store.check("k", Cost(7)).await.unwrap();
    assert!(d.allowed);
    assert_eq!(d.remaining, Tokens(3));
    let d = store.check("k", Cost(11)).await.unwrap();
    assert!(!d.allowed);
    assert_eq!(d.retry_after, None);
}

#[tokio::test]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn reset_restores_the_full_quota() {
    let (_c, url) = redis().await;
    let store = RedisRateLimiter::connect(&url, slow(2)).await.unwrap();
    store.check("k", Cost(2)).await.unwrap();
    assert!(!store.check("k", Cost::ONE).await.unwrap().allowed);
    store.reset("k").await.unwrap();
    assert!(store.check("k", Cost(2)).await.unwrap().allowed);
}

#[tokio::test]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn uses_evalsha_and_recovers_after_script_flush() {
    let (_c, url) = redis().await;
    let store = RedisRateLimiter::connect(&url, slow(100)).await.unwrap();
    let mut admin = raw(&url).await;
    for _ in 0..3 {
        store.check("k", Cost::ONE).await.unwrap();
    }
    let _: () = redis::cmd("SCRIPT")
        .arg("FLUSH")
        .query_async(&mut admin)
        .await
        .unwrap();
    assert!(store.check("k", Cost::ONE).await.unwrap().allowed);

    let stats: String = redis::cmd("INFO")
        .arg("commandstats")
        .query_async(&mut admin)
        .await
        .unwrap();
    let evalsha = stats
        .lines()
        .find(|l| l.starts_with("cmdstat_evalsha:"))
        .expect("EVALSHA was used");
    // 4 checks, all by hash. The first use and the first call after the flush got
    // NOSCRIPT, so the client sent SCRIPT LOAD and retried: 6 EVALSHA, 2 failed.
    assert!(evalsha.contains("calls=6,"), "{evalsha}");
    assert!(evalsha.contains("failed_calls=2"), "{evalsha}");
}

#[tokio::test]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn the_bucket_follows_redis_time_and_expires() {
    let (_c, url) = redis().await;
    let store = RedisRateLimiter::connect(&url, slow(3)).await.unwrap();
    let mut admin = raw(&url).await;
    store.check("k", Cost::ONE).await.unwrap();
    let last: u64 = redis::cmd("HGET")
        .arg("drl:tb:k")
        .arg("last")
        .query_async(&mut admin)
        .await
        .unwrap();
    let (secs, micros): (u64, u64) = redis::cmd("TIME").query_async(&mut admin).await.unwrap();
    let redis_now = secs * 1_000_000 + micros;
    assert!(last <= redis_now && redis_now - last < 5_000_000);

    let ttl: i64 = redis::cmd("PTTL")
        .arg("drl:tb:k")
        .query_async(&mut admin)
        .await
        .unwrap();
    assert!(ttl > 3_599_000 && ttl <= 3_600_001, "ttl {ttl}");
}

#[tokio::test]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn breaker_opens_during_an_outage_and_policies_differ() {
    let (_c, url) = redis().await;
    let options = RedisOptions {
        response_timeout: Duration::from_millis(100),
        ..RedisOptions::default()
    };
    let store = RedisRateLimiter::connect_with(&url, slow(1000), options)
        .await
        .unwrap();
    let clock = Arc::new(ManualClock::new());
    let breaker = CircuitBreaker::new(
        BreakerConfig::new(2, Duration::from_secs(1)).unwrap(),
        Arc::clone(&clock),
    );
    let guarded = GuardedLimiter::new(store, breaker);
    let mut admin = raw(&url).await;

    let ok = guarded
        .check("k", Cost::ONE, FailurePolicy::FailClosed)
        .await;
    assert!(matches!(ok.source, Source::Redis));

    // Outage: Redis stops running scripts (write commands) until unpaused.
    let _: () = redis::cmd("CLIENT")
        .arg("PAUSE")
        .arg(60_000)
        .arg("WRITE")
        .query_async(&mut admin)
        .await
        .unwrap();

    for _ in 0..2 {
        let c = guarded
            .check("k", Cost::ONE, FailurePolicy::FailClosed)
            .await;
        assert!(matches!(c.source, Source::Error(_)), "{:?}", c.source);
        assert!(!c.decision.allowed);
    }
    assert_eq!(guarded.breaker().state(), BreakerState::Open);

    // Open: Redis is not called at all; each route's policy decides.
    let calls = guarded.store().redis_calls();
    let closed = guarded
        .check("k", Cost::ONE, FailurePolicy::FailClosed)
        .await;
    let open = guarded.check("k", Cost::ONE, FailurePolicy::FailOpen).await;
    assert!(matches!(closed.source, Source::BreakerOpen));
    assert!(!closed.decision.allowed);
    assert_eq!(closed.decision.retry_after, Some(Duration::from_secs(1)));
    assert!(open.decision.allowed);
    assert_eq!(guarded.store().redis_calls(), calls);

    // Recovery: Redis is back, the breaker's open time passes, one probe closes it.
    let _: () = redis::cmd("CLIENT")
        .arg("UNPAUSE")
        .query_async(&mut admin)
        .await
        .unwrap();
    clock.advance(Duration::from_secs(1));
    let probe = guarded
        .check("k", Cost::ONE, FailurePolicy::FailClosed)
        .await;
    assert!(matches!(probe.source, Source::Redis), "{:?}", probe.source);
    assert!(probe.decision.allowed);
    assert_eq!(guarded.breaker().state(), BreakerState::Closed);
}

#[tokio::test]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn the_store_protocol_matches_the_simulated_store() {
    let (_c, url) = redis().await;
    let config = slow(10);
    let redis = RedisRateLimiter::connect(&url, config).await.unwrap();
    let mut model = Store::new(config);
    // No refill happens during the test (10 per hour), so the two must agree exactly.
    let ops = [
        StoreOp::Lease { want: 4 },
        StoreOp::Check {
            cost: Cost(3),
            at: None,
        },
        StoreOp::Sync { used: 2 },
        StoreOp::Lease { want: 4 },
        StoreOp::Lease { want: 4 },
        StoreOp::Return { amount: 3 },
        StoreOp::Check {
            cost: Cost(2),
            at: None,
        },
        StoreOp::Check {
            cost: Cost(2),
            at: None,
        },
        StoreOp::Sync { used: 50 },
    ];
    for op in ops {
        let real = redis.apply("k", &op).await.unwrap();
        let sim = model.handle(Nanos::ZERO, "k", &op);
        match (real, sim) {
            (StoreReply::Checked(a), StoreReply::Checked(b)) => {
                assert_eq!(a.allowed, b.allowed, "{op:?}");
                assert_eq!(a.remaining, b.remaining, "{op:?}");
            }
            (StoreReply::Leased { granted: a, .. }, StoreReply::Leased { granted: b, .. }) => {
                assert_eq!(a, b, "{op:?}");
            }
            (a, b) => assert_eq!(a, b, "{op:?}"),
        }
    }
}

#[tokio::test]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn an_empty_lease_says_when_to_come_back() {
    let (_c, url) = redis().await;
    let redis = RedisRateLimiter::connect(&url, RateLimitConfig::per_second(10).unwrap())
        .await
        .unwrap();
    redis
        .apply("k", &StoreOp::Lease { want: 10 })
        .await
        .unwrap();
    match redis.apply("k", &StoreOp::Lease { want: 5 }).await.unwrap() {
        StoreReply::Leased {
            granted: 0,
            retry_after: Some(d),
        } => {
            // About half a second until 5 tokens refill at 10 per second.
            assert!(
                d <= Duration::from_millis(500) && d > Duration::from_millis(400),
                "{d:?}"
            );
        }
        // A refill can land between the two calls on a slow machine.
        StoreReply::Leased { granted, .. } => assert!(granted <= 1),
        other => panic!("{other:?}"),
    }
}
