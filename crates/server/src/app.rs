//! The HTTP application: real routes behind [`RateLimitLayer`], plus decision totals.
//!
//! | Route | Limited | On Redis failure |
//! |-------|---------|------------------|
//! | `GET /api/hello` | policy `public` | fail open |
//! | `POST /api/orders` | policy `orders` | fail closed |
//! | `GET /health`, `GET /metrics` | no | |
//! | `GET /api/metrics` | no | |
//! | everything else | no | the built playground (`web/dist`), if present |

use crate::layer::{Limits, RateLimitLayer, RoutePolicy};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use drl_core::FailurePolicy;
use metrics_exporter_prometheus::PrometheusHandle;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use tower_http::services::ServeDir;

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
        .route("/api/metrics", get(decision_totals))
        .fallback_service(ServeDir::new(web_dir))
        .with_state(state)
}

/// Benchmark-only routes, added when the server runs with `DRL_ADMIN=1`:
/// `GET /admin/strategy` reports the running strategy and its store calls, and
/// `POST /admin/strategy/{name}` switches to another one.
pub fn admin_router(limits: Arc<Limits>) -> Router {
    Router::new()
        .route("/admin/strategy", get(admin_current))
        .route("/admin/strategy/{name}", post(admin_switch))
        .with_state(limits)
}

async fn admin_current(State(limits): State<Arc<Limits>>) -> Response {
    match &limits.backend {
        crate::backend::Backend::Strategy(s) => Json(serde_json::json!({
            "strategy": s.name(),
            "store_calls": s.store_calls(),
        }))
        .into_response(),
        _ => (StatusCode::NOT_FOUND, "not running a coordination strategy").into_response(),
    }
}

async fn admin_switch(
    State(limits): State<Arc<Limits>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    match &limits.backend {
        crate::backend::Backend::Strategy(s) => match s.switch(&name) {
            Ok(()) => (StatusCode::OK, name).into_response(),
            Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
        },
        _ => (StatusCode::NOT_FOUND, "not running a coordination strategy").into_response(),
    }
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
struct DecisionTotals {
    total: u64,
    allowed: u64,
    blocked: u64,
    allow_rate: f64,
}

async fn decision_totals(State(state): State<AppState>) -> Json<DecisionTotals> {
    let (allowed, blocked) = state.limits.stats.totals();
    let total = allowed + blocked;
    let allow_rate = if total > 0 {
        allowed as f64 * 100.0 / total as f64
    } else {
        0.0
    };
    Json(DecisionTotals {
        total,
        allowed,
        blocked,
        allow_rate,
    })
}
