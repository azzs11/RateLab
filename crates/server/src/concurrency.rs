//! Tower middleware that limits requests in flight with a [`ConcurrencyLimiter`].
//!
//! [`ConcurrencyLimitLayer`] admits a request while fewer than the limit are in
//! progress and answers `503 Service Unavailable` straight away otherwise, without
//! calling the route. When an admitted request finishes, its latency goes back to the
//! limiter: a `5xx` answer (such as a `504` timeout) counts as a drop, anything else as
//! a success, and a request cancelled before it finished (the client went away) is not
//! learned from. Every answer carries the current limit in `x-drl-concurrency-limit`.
//!
//! Put it outside the rate limit layer: overload protection applies to all callers,
//! and a shed request should not spend a caller's quota.

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Request, Response, StatusCode};
use ratelab::concurrency::{
    AimdConfig, Completion, Concurrency, ConcurrencyLimiter, Gradient2Config, Ticket,
};
use ratelab::{Clock, MonotonicClock, Nanos};
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// Parses a `--concurrency` value: `off`, `fixed:N`, `aimd` or `gradient2` (the last two
/// with their default settings).
///
/// ```
/// use ratelab::concurrency::Concurrency;
/// use server::concurrency::parse;
///
/// assert_eq!(parse("off"), Ok(None));
/// assert_eq!(parse("fixed:32"), Ok(Some(Concurrency::Fixed(32))));
/// assert_eq!(parse("gradient2").unwrap().unwrap().name(), "gradient2");
/// assert!(parse("fixed:0").is_err());
/// ```
pub fn parse(spec: &str) -> Result<Option<Concurrency>, String> {
    match spec.trim() {
        "off" | "" => Ok(None),
        "aimd" => Ok(Some(Concurrency::Aimd(AimdConfig::default()))),
        "gradient2" => Ok(Some(Concurrency::Gradient2(Gradient2Config::default()))),
        other => match other.strip_prefix("fixed:").map(str::parse::<usize>) {
            Some(Ok(n)) if n > 0 => Ok(Some(Concurrency::Fixed(n))),
            _ => Err(format!(
                "unknown concurrency limit {other:?} (off, fixed:N, aimd or gradient2)"
            )),
        },
    }
}

/// Header with the concurrency limit when the request arrived.
pub const LIMIT_HEADER: &str = "x-drl-concurrency-limit";

/// Limits requests in flight on the wrapped service.
#[derive(Clone)]
pub struct ConcurrencyLimitLayer {
    limiter: Arc<ConcurrencyLimiter>,
    clock: Arc<dyn Clock>,
}

impl ConcurrencyLimitLayer {
    /// A layer using `limiter` and the real clock.
    pub fn new(limiter: Arc<ConcurrencyLimiter>) -> Self {
        Self::with_clock(limiter, Arc::new(MonotonicClock::new()))
    }

    /// A layer using `limiter` and `clock` (tests pass a `ManualClock`).
    pub fn with_clock(limiter: Arc<ConcurrencyLimiter>, clock: Arc<dyn Clock>) -> Self {
        Self { limiter, clock }
    }
}

impl<S> Layer<S> for ConcurrencyLimitLayer {
    type Service = ConcurrencyLimit<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ConcurrencyLimit {
            inner,
            layer: self.clone(),
        }
    }
}

/// The service produced by [`ConcurrencyLimitLayer`].
#[derive(Clone)]
pub struct ConcurrencyLimit<S> {
    inner: S,
    layer: ConcurrencyLimitLayer,
}

/// Gives the slot back if the request is cancelled before it finishes.
struct Slot {
    limiter: Arc<ConcurrencyLimiter>,
    ticket: Option<Ticket>,
}

impl Slot {
    fn finish(mut self, now: Nanos, start: Nanos, completion: Completion) {
        if let Some(t) = self.ticket.take() {
            self.limiter
                .release(t, now, now.saturating_since(start), completion);
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(t) = self.ticket.take() {
            self.limiter.release(
                t,
                Nanos::ZERO,
                std::time::Duration::ZERO,
                Completion::Ignored,
            );
        }
    }
}

fn limit_value(limit: usize) -> HeaderValue {
    HeaderValue::from(u64::try_from(limit).unwrap_or(u64::MAX))
}

fn overloaded(limit: usize) -> Response<Body> {
    let body = serde_json::json!({ "error": "overloaded" });
    let mut response = Response::new(Body::from(body.to_string()));
    *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
        .headers_mut()
        .insert(HeaderName::from_static(LIMIT_HEADER), limit_value(limit));
    response
}

impl<S> Service<Request<Body>> for ConcurrencyLimit<S>
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
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        let limiter = Arc::clone(&self.layer.limiter);
        let clock = Arc::clone(&self.layer.clock);
        Box::pin(async move {
            let limit = limiter.limit();
            let Some(ticket) = limiter.try_acquire() else {
                metrics::counter!("drl_concurrency_total", "outcome" => "shed").increment(1);
                return Ok(overloaded(limit));
            };
            let slot = Slot {
                limiter: Arc::clone(&limiter),
                ticket: Some(ticket),
            };
            let start = clock.now();
            let mut response = inner.call(request).await?;
            let completion = if response.status().is_server_error() {
                Completion::Dropped
            } else {
                Completion::Success
            };
            slot.finish(clock.now(), start, completion);
            let outcome = match completion {
                Completion::Dropped => "dropped",
                _ => "ok",
            };
            metrics::counter!("drl_concurrency_total", "outcome" => outcome).increment(1);
            metrics::gauge!("drl_concurrency_limit").set(limiter.limit() as f64);
            response
                .headers_mut()
                .insert(HeaderName::from_static(LIMIT_HEADER), limit_value(limit));
            Ok(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get;
    use ratelab::ManualClock;
    use std::time::Duration;
    use tokio::sync::{Notify, oneshot};
    use tower::ServiceExt;

    fn get_req(path: &str) -> Request<Body> {
        Request::builder().uri(path).body(Body::empty()).unwrap()
    }

    fn aimd(initial: usize) -> Arc<ConcurrencyLimiter> {
        Arc::new(ConcurrencyLimiter::new(Concurrency::Aimd(AimdConfig {
            initial,
            latency_threshold: Duration::from_millis(100),
            ..AimdConfig::default()
        })))
    }

    #[tokio::test]
    async fn sheds_above_the_limit_with_503() {
        let limiter = aimd(1);
        let gate = Arc::new(Notify::new());
        let (entered_tx, entered_rx) = oneshot::channel::<()>();
        let entered = Arc::new(std::sync::Mutex::new(Some(entered_tx)));
        let g = Arc::clone(&gate);
        let app = Router::new()
            .route(
                "/slow",
                get(move || {
                    let (g, entered) = (Arc::clone(&g), Arc::clone(&entered));
                    async move {
                        if let Some(tx) = entered.lock().unwrap().take() {
                            let _ = tx.send(());
                        }
                        g.notified().await;
                        "done"
                    }
                }),
            )
            .layer(ConcurrencyLimitLayer::new(Arc::clone(&limiter)));

        let first = tokio::spawn(app.clone().oneshot(get_req("/slow")));
        entered_rx.await.unwrap();
        let shed = app.clone().oneshot(get_req("/slow")).await.unwrap();
        assert_eq!(shed.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(shed.headers()[LIMIT_HEADER], "1");

        gate.notify_one();
        let ok = first.await.unwrap().unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        assert_eq!(limiter.inflight(), 0);
        assert_eq!(limiter.limit(), 2, "a used limit grows on success");
    }

    #[tokio::test]
    async fn server_errors_and_slow_answers_back_off() {
        let limiter = aimd(10);
        let clock = Arc::new(ManualClock::new());
        let c = Arc::clone(&clock);
        let app = Router::new()
            .route(
                "/fail",
                get(|| async { (StatusCode::GATEWAY_TIMEOUT, "late") }),
            )
            .route(
                "/slow",
                get(move || {
                    let c = Arc::clone(&c);
                    async move {
                        c.advance(Duration::from_millis(150));
                        "slow"
                    }
                }),
            )
            .layer(ConcurrencyLimitLayer::with_clock(
                Arc::clone(&limiter),
                clock,
            ));
        let r = app.clone().oneshot(get_req("/fail")).await.unwrap();
        assert_eq!(r.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(limiter.limit(), 9);
        let r = app.clone().oneshot(get_req("/slow")).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(limiter.limit(), 8, "150 ms is over the 100 ms threshold");
    }

    #[tokio::test]
    async fn a_cancelled_request_frees_its_slot_without_teaching() {
        let limiter = aimd(1);
        let app = Router::new()
            .route("/hang", get(std::future::pending::<&'static str>))
            .layer(ConcurrencyLimitLayer::new(Arc::clone(&limiter)));
        let mut call = Box::pin(app.clone().oneshot(get_req("/hang")));
        // Poll once: the request is admitted and waits forever.
        std::future::poll_fn(|cx| {
            assert!(call.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert_eq!(limiter.inflight(), 1);
        drop(call); // the client goes away
        assert_eq!(limiter.inflight(), 0);
        assert_eq!(limiter.limit(), 1);
    }
}
