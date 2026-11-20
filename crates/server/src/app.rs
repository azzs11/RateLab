//! The HTTP application: real routes behind [`RateLimitLayer`], plus the dashboard API.
//!
//! | Route | Limited | On Redis failure |
//! |-------|---------|------------------|
//! | `GET /api/hello` | policy `public` | fail open |
//! | `POST /api/orders` | policy `orders` | fail closed |
//! | `GET /health`, `GET /metrics` | no | |
//! | `GET /api/metrics`, `POST /api/test` | no (dashboard) | |
//! | everything else | no | static files from the web directory |

use crate::layer::{Limits, RateLimitLayer, RoutePolicy};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use drl_core::{Algorithm, Cost, FailurePolicy, MonotonicClock, RateLimitConfig};
use metrics_exporter_prometheus::PrometheusHandle;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower_http::services::ServeDir;

/// Largest burst `/api/test` will run in one call.
const MAX_TEST_REQUESTS: u32 = 100_000;

#[derive(Clone)]
struct AppState {
    limits: Arc<Limits>,
    prometheus: Option<PrometheusHandle>,
}

/// The policies the two limited routes use.
pub fn route_policies() -> [RoutePolicy; 2] {
    [
        RoutePolicy::new("public", FailurePolicy::FailOpen),
        RoutePolicy::new("orders", FailurePolicy::FailClosed),
    ]
}

/// Builds the router. `prometheus` is `None` when no recorder is installed (tests).
pub fn router(
    limits: Arc<Limits>,
    prometheus: Option<PrometheusHandle>,
    web_dir: impl AsRef<Path>,
) -> Router {
    let [public, orders] = route_policies();
    let state = AppState {
        limits: Arc::clone(&limits),
        prometheus,
    };
    Router::new()
        .route(
            "/api/hello",
            get(hello).layer(RateLimitLayer::new(Arc::clone(&limits), public)),
        )
        .route(
            "/api/orders",
            post(create_order).layer(RateLimitLayer::new(Arc::clone(&limits), orders)),
        )
        .route("/health", get(|| async { "ok" }))
        .route("/metrics", get(prometheus_metrics))
        .route("/api/metrics", get(dashboard_metrics))
        .route("/api/test", post(dashboard_test))
        .fallback_service(ServeDir::new(web_dir))
        .with_state(state)
}

async fn hello(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "message": "hello", "node": state.limits.node_id }))
}

async fn create_order(State(state): State<AppState>) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::CREATED,
        Json(serde_json::json!({ "accepted": true, "node": state.limits.node_id })),
    )
}

async fn prometheus_metrics(State(state): State<AppState>) -> Response {
    match &state.prometheus {
        Some(handle) => handle.render().into_response(),
        None => (StatusCode::NOT_FOUND, "metrics recorder not installed").into_response(),
    }
}

#[derive(Serialize)]
struct DashboardMetrics {
    total: u64,
    allowed: u64,
    blocked: u64,
    allow_rate: f64,
}

async fn dashboard_metrics(State(state): State<AppState>) -> Json<DashboardMetrics> {
    let (allowed, blocked) = state.limits.stats.totals();
    let total = allowed + blocked;
    let allow_rate = if total > 0 {
        allowed as f64 * 100.0 / total as f64
    } else {
        0.0
    };
    Json(DashboardMetrics {
        total,
        allowed,
        blocked,
        allow_rate,
    })
}

#[derive(Deserialize)]
struct TestRequest {
    algorithm: String,
    max_requests: u64,
    window_seconds: u64,
    num_requests: u32,
}

#[derive(Serialize)]
struct TestResponse {
    allowed: u32,
    blocked: u32,
    duration_ms: f64,
    requests_per_sec: f64,
    results: Vec<bool>,
}

/// Runs a burst against a fresh in-process limiter, for the dashboard.
async fn dashboard_test(
    State(state): State<AppState>,
    Json(req): Json<TestRequest>,
) -> Result<Json<TestResponse>, (StatusCode, String)> {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    let algorithm: Algorithm = req.algorithm.parse().map_err(|e| bad(format!("{e}")))?;
    let config = RateLimitConfig::new(req.max_requests, Duration::from_secs(req.window_seconds))
        .map_err(|e| bad(e.to_string()))?;
    if req.num_requests > MAX_TEST_REQUESTS {
        return Err(bad(format!(
            "num_requests must be at most {MAX_TEST_REQUESTS}"
        )));
    }
    let limiter = algorithm.build(config, MonotonicClock::new());

    let start = Instant::now();
    let results: Vec<bool> = (0..req.num_requests)
        .map(|_| {
            let allowed = limiter.check("test_user", Cost::ONE).allowed;
            state.limits.stats.count(allowed);
            allowed
        })
        .collect();
    let secs = start.elapsed().as_secs_f64();
    let allowed = results.iter().filter(|&&a| a).count() as u32;
    Ok(Json(TestResponse {
        allowed,
        blocked: req.num_requests - allowed,
        duration_ms: secs * 1000.0,
        requests_per_sec: if secs > 0.0 {
            f64::from(req.num_requests) / secs
        } else {
            0.0
        },
        results,
    }))
}
