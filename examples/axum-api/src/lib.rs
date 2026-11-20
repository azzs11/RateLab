//! A small Axum API protected by `ratelab`, in two middleware functions:
//!
//! 1. **A concurrency limit** (Gradient2): requests beyond what the service can work on
//!    at once get `503` straight away instead of queueing.
//! 2. **A rate limit per API key** (GCRA): over the limit, `429` with `Retry-After`; every
//!    answer carries the IETF `RateLimit-Policy` and `RateLimit` headers.
//!
//! The limiter is passed in, so the tests drive it with a manual clock.

#![forbid(unsafe_code)]

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use ratelab::concurrency::{Completion, ConcurrencyLimiter};
use ratelab::{Cost, RateLimitConfig, RateLimiter};
use std::sync::Arc;
use std::time::Instant;

/// What the middleware shares.
#[derive(Clone)]
pub struct Limits {
    /// The per-key rate limit.
    pub rate: Arc<dyn RateLimiter>,
    /// Its configuration, for the headers.
    pub config: RateLimitConfig,
    /// The concurrency limit.
    pub concurrency: Arc<ConcurrencyLimiter>,
    /// A fixed starting point: the concurrency limiter reads time as "since origin".
    pub origin: Instant,
}

/// Whole seconds, rounded up, so a client that waits as told never retries early.
fn secs_up(d: std::time::Duration) -> u64 {
    d.as_nanos()
        .div_ceil(1_000_000_000)
        .try_into()
        .unwrap_or(u64::MAX)
}

fn header(map: &mut HeaderMap, name: &'static str, value: String) {
    if let Ok(v) = HeaderValue::from_str(&value) {
        map.insert(name, v);
    }
}

async fn rate_limit(State(limits): State<Limits>, request: Request, next: Next) -> Response {
    let key = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("anonymous")
        .to_owned();
    let d = limits.rate.check(&key, Cost::ONE);
    let mut response = if d.allowed {
        next.run(request).await
    } else {
        let mut r = (StatusCode::TOO_MANY_REQUESTS, "slow down\n").into_response();
        if let Some(wait) = d.retry_after {
            header(r.headers_mut(), "retry-after", secs_up(wait).to_string());
        }
        r
    };
    let c = limits.config;
    let h = response.headers_mut();
    header(
        h,
        "ratelimit-policy",
        format!("\"default\";q={};w={}", c.limit(), secs_up(c.window())),
    );
    header(
        h,
        "ratelimit",
        format!(
            "\"default\";r={};t={}",
            d.remaining.0,
            secs_up(d.reset_after)
        ),
    );
    response
}

async fn concurrency_limit(State(limits): State<Limits>, request: Request, next: Next) -> Response {
    let Some(ticket) = limits.concurrency.try_acquire() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "overloaded, try again shortly\n",
        )
            .into_response();
    };
    let start = Instant::now();
    let response = next.run(request).await;
    let completion = if response.status().is_server_error() {
        Completion::Dropped
    } else {
        Completion::Success
    };
    let now = ratelab::Nanos(u64::try_from(limits.origin.elapsed().as_nanos()).unwrap_or(u64::MAX));
    limits
        .concurrency
        .release(ticket, now, start.elapsed(), completion);
    response
}

async fn hello() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "message": "hello" }))
}

/// The router: `GET /hello` behind both limits; `GET /health` unlimited.
pub fn app(limits: Limits) -> Router {
    let api = Router::new()
        .route("/hello", get(hello))
        .layer(middleware::from_fn_with_state(limits.clone(), rate_limit))
        .layer(middleware::from_fn_with_state(limits, concurrency_limit));
    api.route("/health", get(|| async { "ok" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use ratelab::concurrency::Concurrency;
    use ratelab::{Gcra, ManualClock};
    use std::time::Duration;
    use tower::ServiceExt;

    fn limits(clock: &Arc<ManualClock>, in_flight: usize) -> Limits {
        let config = RateLimitConfig::per_second(2).unwrap();
        Limits {
            rate: Arc::new(Gcra::with_clock(config, Arc::clone(clock))),
            config,
            concurrency: Arc::new(ConcurrencyLimiter::new(Concurrency::Fixed(in_flight))),
            origin: Instant::now(),
        }
    }

    fn get(path: &str, key: &str) -> Request {
        Request::get(path)
            .header("x-api-key", key)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn over_the_limit_is_429_with_retry_after_and_headers() {
        let clock = Arc::new(ManualClock::new());
        let app = app(limits(&clock, 10));
        for _ in 0..2 {
            let r = app.clone().oneshot(get("/hello", "alice")).await.unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            assert_eq!(r.headers()["ratelimit-policy"], "\"default\";q=2;w=1");
        }
        let r = app.clone().oneshot(get("/hello", "alice")).await.unwrap();
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r.headers()["retry-after"], "1");
        assert_eq!(r.headers()["ratelimit"], "\"default\";r=0;t=1");
        // Another key has its own budget.
        let r = app.clone().oneshot(get("/hello", "bob")).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        // Half a second later one request fits again.
        clock.advance(Duration::from_millis(500));
        let r = app.oneshot(get("/hello", "alice")).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_is_never_limited() {
        let clock = Arc::new(ManualClock::new());
        let app = app(limits(&clock, 10));
        for _ in 0..5 {
            let r = app.clone().oneshot(get("/health", "alice")).await.unwrap();
            assert_eq!(r.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn requests_beyond_the_concurrency_limit_get_503() {
        let clock = Arc::new(ManualClock::new());
        let l = limits(&clock, 1);
        let held = l.concurrency.try_acquire().unwrap(); // one request already in flight
        let r = app(l.clone())
            .oneshot(get("/hello", "alice"))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        l.concurrency
            .release(held, ratelab::Nanos(0), Duration::ZERO, Completion::Ignored);
        let r = app(l).oneshot(get("/hello", "alice")).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }
}
