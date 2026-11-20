//! Coordination strategies against a real Redis: three nodes in one process sharing it.
//! Needs Docker: `cargo test -p server -- --ignored` (CI runs it).

use drl_core::{Cost, FailurePolicy, RateLimitConfig};
use drl_redis_store::RedisRateLimiter;
use server::strategy::{STRATEGIES, StrategyBackend};
use std::sync::Arc;
use std::time::Duration;
use testcontainers_modules::redis::{REDIS_PORT, Redis};
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

/// 12 per hour: nothing refills while the test runs.
fn config() -> RateLimitConfig {
    RateLimitConfig::new(12, Duration::from_secs(3600)).unwrap()
}

async fn cluster(url: &str, strategy: &str) -> Vec<Arc<StrategyBackend>> {
    let mut nodes = Vec::new();
    for _ in 0..3 {
        let store = RedisRateLimiter::connect(url, config()).await.unwrap();
        nodes.push(
            StrategyBackend::start(
                strategy,
                store,
                3,
                FailurePolicy::FailClosed,
                Duration::from_millis(500),
            )
            .unwrap(),
        );
    }
    nodes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn strategies_share_one_limit_through_redis() {
    let container = Redis::default()
        .with_tag("7.4-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(REDIS_PORT).await.unwrap();
    let url = format!("redis://{}:{port}/", container.get_host().await.unwrap());

    for (i, &name) in STRATEGIES.iter().enumerate() {
        let nodes = cluster(&url, name).await;
        let key = format!("k{i}");
        let mut admitted = 0;
        // 60 requests, spread over the three nodes.
        for r in 0..60 {
            if nodes[r % 3].check(&key, Cost::ONE).await.allowed {
                admitted += 1;
            }
        }
        match name {
            // Every token comes from Redis: never more than the global 12.
            "central_exact" | "fixed_leasing" | "adaptive_leasing" => {
                assert_eq!(admitted, 12, "{name}");
            }
            // 12 / 3 nodes = 4 each, decided locally.
            "local_split" => assert_eq!(admitted, 12, "{name}"),
            // Decides locally before syncing: may over-admit, never under the limit.
            "async_sync" => assert!(admitted >= 12, "{name}: {admitted}"),
            _ => unreachable!(),
        }
        let calls: u64 = nodes.iter().map(|n| n.store_calls()).sum();
        if name == "local_split" {
            assert_eq!(calls, 0);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn a_node_can_switch_strategy() {
    let container = Redis::default()
        .with_tag("7.4-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(REDIS_PORT).await.unwrap();
    let url = format!("redis://{}:{port}/", container.get_host().await.unwrap());
    let nodes = cluster(&url, "local_split").await;
    assert_eq!(nodes[0].name(), "local_split");
    nodes[0].switch("central_exact").unwrap();
    assert_eq!(nodes[0].name(), "central_exact");
    assert!(nodes[0].check("x", Cost::ONE).await.allowed);
    assert_eq!(nodes[0].store_calls(), 1);
    assert!(nodes[0].switch("magic").is_err());
}
