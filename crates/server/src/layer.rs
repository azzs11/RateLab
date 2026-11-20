//! Tower middleware that rate limits a route.
//!
//! [`RateLimitLayer`] wraps any Axum route. For each request it:
//!
//! 1. finds the caller's key: the `x-api-key` header, else the peer IP, else
//!    `anonymous`;
//! 2. asks the shared [`Backend`] under the key `"{policy}:{caller}"`, so each route
//!    policy has its own budget per caller;
//! 3. on a deny, answers `429 Too Many Requests` without calling the route;
//! 4. adds `RateLimit-Policy`, `RateLimit` (and `Retry-After` on 429) headers, plus
//!    `x-drl-source` and `x-drl-node` so load tests can see who decided.
//!
//! When Redis is unavailable the route's [`FailurePolicy`] decides, and the
//! `RateLimit` headers are left out because no limiter state was read.

use crate::backend::{Backend, Outcome};
use crate::headers;
use crate::metrics::{self, Stats};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode};
use drl_core::{Cost, FailurePolicy};
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;
use tower::{Layer, Service};

/// Longest `x-api-key` accepted as a key; longer values are treated as absent.
const MAX_KEY_LEN: usize = 128;

/// Header with the source of the decision (`local`, `redis`, `breaker_open`, `redis_error`).
pub const SOURCE_HEADER: &str = "x-drl-source";
/// Header with the node that answered.
pub const NODE_HEADER: &str = "x-drl-node";

/// One route's limiting rules.
#[derive(Debug, Clone)]
pub struct RoutePolicy {
    /// Name in headers, metrics and the storage key.
    pub name: String,
    /// What happens when Redis cannot answer.
    pub failure: FailurePolicy,
    /// Units each request costs.
    pub cost: Cost,
}

impl RoutePolicy {
    /// A policy charging one unit per request.
    pub fn new(name: impl Into<String>, failure: FailurePolicy) -> Self {
        Self {
            name: name.into(),
            failure,
            cost: Cost::ONE,
        }
    }
}

/// Everything the layer shares between requests.
pub struct Limits {
    /// The backend that decides.
    pub backend: Backend,
    /// Dashboard totals.
    pub stats: Stats,
    /// This node's id, sent in `x-drl-node`.
    pub node_id: String,
}

/// Rate limits the wrapped service with a [`RoutePolicy`].
#[derive(Clone)]
pub struct RateLimitLayer {
    limits: Arc<Limits>,
    policy: Arc<RoutePolicy>,
    policy_value: HeaderValue,
}

impl RateLimitLayer {
    /// A layer enforcing `policy` with the shared `limits`.
    pub fn new(limits: Arc<Limits>, policy: RoutePolicy) -> Self {
        let policy_value = header_value(&headers::policy_header(
            &policy.name,
            limits.backend.config(),
        ));
        Self {
            limits,
            policy: Arc::new(policy),
            policy_value,
        }
    }
}

impl<S> Layer<S> for RateLimitLayer {
    type Service = RateLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RateLimit {
            inner,
            layer: self.clone(),
        }
    }
}

/// The service produced by [`RateLimitLayer`].
#[derive(Clone)]
pub struct RateLimit<S> {
    inner: S,
    layer: RateLimitLayer,
}

impl<S> Service<Request<Body>> for RateLimit<S>
where
    S: Service<Request<Body>, Response = Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Body>, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        // Take the service that was polled ready; leave a fresh clone in its place.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let layer = self.layer.clone();
        Box::pin(async move {
            let policy = &layer.policy;
            let key = format!("{}:{}", policy.name, caller_key(&request));
            let start = Instant::now();
            let outcome = layer
                .limits
                .backend
                .check(&key, policy.cost, policy.failure)
                .await;
            let allowed = outcome.decision.allowed;
            metrics::record(&policy.name, allowed, outcome.source, start.elapsed());
            layer.limits.stats.count(allowed);

            let mut response = if allowed {
                inner.call(request).await?
            } else {
                too_many_requests(&outcome)
            };
            add_headers(response.headers_mut(), &layer, &outcome);
            Ok(response)
        })
    }
}

/// The caller's identity: `x-api-key`, else the peer IP, else `anonymous`.
pub fn caller_key<B>(request: &Request<B>) -> String {
    if let Some(key) = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|k| !k.is_empty() && k.len() <= MAX_KEY_LEN)
    {
        return key.to_owned();
    }
    if let Some(ConnectInfo(addr)) = request.extensions().get::<ConnectInfo<SocketAddr>>() {
        return addr.ip().to_string();
    }
    "anonymous".to_owned()
}

fn too_many_requests(outcome: &Outcome) -> Response<Body> {
    let retry_after_ms = outcome
        .decision
        .retry_after
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let body = serde_json::json!({
        "error": "rate_limited",
        "retry_after_ms": retry_after_ms,
    });
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if let Some(secs) = headers::retry_after_header(&outcome.decision) {
        response
            .headers_mut()
            .insert(axum::http::header::RETRY_AFTER, HeaderValue::from(secs));
    }
    response
}

fn add_headers(map: &mut HeaderMap, layer: &RateLimitLayer, outcome: &Outcome) {
    if outcome.source.is_enforced() {
        map.insert(
            HeaderName::from_static("ratelimit-policy"),
            layer.policy_value.clone(),
        );
        map.insert(
            HeaderName::from_static("ratelimit"),
            header_value(&headers::ratelimit_header(
                &layer.policy.name,
                &outcome.decision,
            )),
        );
    }
    map.insert(
        HeaderName::from_static(SOURCE_HEADER),
        HeaderValue::from_static(outcome.source.name()),
    );
    map.insert(
        HeaderName::from_static(NODE_HEADER),
        header_value(&layer.limits.node_id),
    );
}

/// A header value from our own formatted text; falls back to empty if it is not
/// valid (policy names and node ids come from the operator's config).
fn header_value(s: &str) -> HeaderValue {
    HeaderValue::from_str(s).unwrap_or_else(|_| HeaderValue::from_static(""))
}
