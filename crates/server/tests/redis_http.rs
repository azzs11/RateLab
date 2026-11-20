//! Per-route failure policies over HTTP, against a real Redis (testcontainers).
//! Needs Docker: `cargo test -p server -- --ignored` (CI runs it).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use drl_redis_store::{GuardedLimiter, RedisOptions, RedisRateLimiter};
use ratelab::{BreakerConfig, CircuitBreaker, MonotonicClock, RateLimitConfig};
use server::app;
use server::backend::Backend;
use server::layer::Limits;
use std::sync::Arc;
use std::time::Duration;
use testcontainers_modules::redis::{REDIS_PORT, Redis};
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tower::ServiceExt;

fn request(method: &str, path: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("x-api-key", "k")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
#[ignore = "needs Docker; CI runs it with --ignored"]
async fn public_fails_open_and_orders_fail_closed_during_an_outage() {
    let container = Redis::default()
        .with_tag("7.4-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(REDIS_PORT).await.unwrap();
    let url = format!("redis://{}:{port}/", container.get_host().await.unwrap());

    let config = RateLimitConfig::per_minute(100).unwrap();
    let options = RedisOptions {
        response_timeout: Duration::from_millis(100),
        ..RedisOptions::default()
    };
    let store = RedisRateLimiter::connect_with(&url, config, options)
        .await
        .unwrap();
    // A long open time so the breaker stays open for the rest of the test.
    let breaker = CircuitBreaker::new(
        BreakerConfig::new(1, Duration::from_secs(600)).unwrap(),
        MonotonicClock::new(),
    );
    let limits = Arc::new(Limits {
        backend: Backend::Redis(GuardedLimiter::new(store, breaker)),
        stats: Default::default(),
        node_id: "n1".to_owned(),
    });
    let router = app::router(limits, None, ".");

    let r = router
        .clone()
        .oneshot(request("GET", "/api/hello"))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["x-drl-source"], "redis");
    assert_eq!(r.headers()["ratelimit"], r#""public";r=99;t=1"#);

    let mut admin = redis::Client::open(url.as_str())
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let _: () = redis::cmd("CLIENT")
        .arg("PAUSE")
        .arg(60_000)
        .arg("WRITE")
        .query_async(&mut admin)
        .await
        .unwrap();

    // First failure times out and opens the breaker.
    let r = router
        .clone()
        .oneshot(request("POST", "/api/orders"))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.headers()["x-drl-source"], "redis_error");

    let r = router
        .clone()
        .oneshot(request("GET", "/api/hello"))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["x-drl-source"], "breaker_open");
    assert!(r.headers().get("ratelimit").is_none());

    let r = router
        .oneshot(request("POST", "/api/orders"))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.headers()["x-drl-source"], "breaker_open");
    assert_eq!(r.headers()["retry-after"], "600");

    let _: () = redis::cmd("CLIENT")
        .arg("UNPAUSE")
        .query_async(&mut admin)
        .await
        .unwrap();
}
