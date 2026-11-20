//! The router end to end, on an in-process limiter driven by a manual clock.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use drl_core::{Algorithm, ManualClock, RateLimitConfig};
use server::app;
use server::backend::Backend;
use server::layer::{Limits, caller_key};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

/// A stand-in for the built playground.
fn web_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("drl-web-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.html"), "<!doctype html><title>t</title>").unwrap();
    dir
}

/// 3 per minute: one token refills every 20 s.
fn setup() -> (Router, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new());
    let config = RateLimitConfig::per_minute(3).unwrap();
    let limits = Arc::new(Limits {
        backend: Backend::Local {
            limiter: Algorithm::TokenBucket.build(config, Arc::clone(&clock)),
            config,
        },
        stats: Default::default(),
        node_id: "node-test".to_owned(),
    });
    (app::router(limits, None, web_dir()), clock)
}

fn get(path: &str, api_key: &str) -> Request<Body> {
    Request::get(path)
        .header("x-api-key", api_key)
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, api_key: &str) -> Request<Body> {
    Request::post(path)
        .header("x-api-key", api_key)
        .body(Body::empty())
        .unwrap()
}

fn header<'a>(response: &'a axum::response::Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).map(|v| v.to_str().unwrap())
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn allowed_requests_carry_ietf_headers() {
    let (app, _clock) = setup();
    let r = app.oneshot(get("/api/hello", "alice")).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(header(&r, "ratelimit-policy"), Some(r#""public";q=3;w=60"#));
    assert_eq!(header(&r, "ratelimit"), Some(r#""public";r=2;t=20"#));
    assert_eq!(header(&r, "x-drl-source"), Some("local"));
    assert_eq!(header(&r, "x-drl-node"), Some("node-test"));
    assert_eq!(header(&r, "retry-after"), None);
    assert_eq!(body_json(r).await["node"], "node-test");
}

#[tokio::test]
async fn over_the_limit_gets_429_with_exact_retry_after() {
    let (app, clock) = setup();
    for _ in 0..3 {
        let r = app.clone().oneshot(get("/api/hello", "bob")).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }
    let r = app.clone().oneshot(get("/api/hello", "bob")).await.unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(header(&r, "retry-after"), Some("20"));
    assert_eq!(header(&r, "ratelimit"), Some(r#""public";r=0;t=60"#));
    let body = body_json(r).await;
    assert_eq!(body["error"], "rate_limited");
    assert_eq!(body["retry_after_ms"], 20_000);

    // Waiting exactly Retry-After is enough.
    clock.advance(Duration::from_secs(20) - Duration::from_nanos(1));
    let r = app.clone().oneshot(get("/api/hello", "bob")).await.unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    clock.advance(Duration::from_nanos(1));
    let r = app.oneshot(get("/api/hello", "bob")).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}

#[tokio::test]
async fn callers_and_routes_have_separate_budgets() {
    let (app, _clock) = setup();
    for _ in 0..3 {
        app.clone()
            .oneshot(get("/api/hello", "carol"))
            .await
            .unwrap();
    }
    let r = app
        .clone()
        .oneshot(get("/api/hello", "carol"))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);

    let r = app
        .clone()
        .oneshot(get("/api/hello", "dave"))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let r = app.oneshot(post("/api/orders", "carol")).await.unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    assert_eq!(header(&r, "ratelimit"), Some(r#""orders";r=2;t=20"#));
}

#[tokio::test]
async fn health_and_metrics_are_not_limited() {
    let (app, _clock) = setup();
    for _ in 0..10 {
        let r = app.clone().oneshot(get("/health", "erin")).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(header(&r, "ratelimit"), None);
    }
    // No recorder installed in tests.
    let r = app.oneshot(get("/metrics", "erin")).await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn metrics_api_counts_decisions() {
    let (app, _clock) = setup();
    for _ in 0..4 {
        app.clone()
            .oneshot(get("/api/hello", "frank"))
            .await
            .unwrap();
    }
    let r = app.oneshot(get("/api/metrics", "frank")).await.unwrap();
    let m = body_json(r).await;
    assert_eq!(m["total"], 4);
    assert_eq!(m["allowed"], 3);
    assert_eq!(m["blocked"], 1);
}

#[tokio::test]
async fn serves_the_built_playground() {
    let (app, _clock) = setup();
    let r = app
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(header(&r, "content-type").unwrap().starts_with("text/html"));
}

#[test]
fn caller_key_prefers_api_key_then_peer_ip() {
    let with_key = Request::get("/")
        .header("x-api-key", " k1 ")
        .body(())
        .unwrap();
    assert_eq!(caller_key(&with_key), "k1");

    let mut by_ip = Request::get("/").body(()).unwrap();
    let addr: SocketAddr = "10.0.0.7:5555".parse().unwrap();
    by_ip.extensions_mut().insert(ConnectInfo(addr));
    assert_eq!(caller_key(&by_ip), "10.0.0.7");

    let too_long = Request::get("/")
        .header("x-api-key", "x".repeat(129))
        .body(())
        .unwrap();
    assert_eq!(caller_key(&too_long), "anonymous");
}
