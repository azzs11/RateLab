//! Token-aware limiting for LLM traffic: a Tower layer in front of a model endpoint,
//! and a mock model that streams tokens.
//!
//! [`TokenLimitLayer`] reads an OpenAI-style JSON body (`model`, `max_tokens`, and the
//! prompt's token count in `prompt_tokens`; a real gateway would run the tokenizer),
//! asks the [`TokenLimiter`] whether the caller may spend that much, and answers
//! `429` with `Retry-After` if not. An admitted request is passed on and its streamed
//! answer is counted as it flows back (one token per server-sent `data:` event, as
//! the mock and OpenAI-style APIs send them). When the stream ends, or the client goes
//! away, the count is reported to the limiter, which reconciles the charge.
//!
//! [`mock_router`] serves `POST /v1/completions`: it waits `first_token`, then streams
//! one event per token every `per_token`, then `data: [DONE]`. The answer's length is
//! `completion_tokens` from the body when given (to replay a trace, as the Splitwise
//! authors did), and otherwise random up to `max_tokens`.

use crate::layer::caller_key;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, Request, Response, StatusCode, header};
use axum::routing::post;
use futures_util::StreamExt;
use ratelab::Clock;
use ratelab::tokens::{TokenLimiter, TokenRequest, TokenTicket};
use serde::Deserialize;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tower::{Layer, Service};

/// Largest request body read, bytes.
const MAX_BODY: usize = 1 << 20;
/// Output units the caller owes, on `429`s.
pub const DEBT_HEADER: &str = "x-drl-token-debt";
/// Input and output units charged at admission.
pub const CHARGED_HEADER: &str = "x-drl-tokens-charged";

/// The fields of a completion request the limiter needs.
#[derive(Debug, Clone, Deserialize)]
pub struct CompletionRequest {
    /// Model name.
    pub model: String,
    /// Prompt length in tokens.
    pub prompt_tokens: u64,
    /// Longest answer wanted.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Mock only: how long the answer must be.
    #[serde(default)]
    pub completion_tokens: Option<u64>,
}

/// Limits a model endpoint by tokens.
pub struct TokenLimitLayer<C: Clock> {
    limiter: Arc<TokenLimiter<C>>,
}

impl<C: Clock> Clone for TokenLimitLayer<C> {
    fn clone(&self) -> Self {
        Self {
            limiter: Arc::clone(&self.limiter),
        }
    }
}

impl<C: Clock> TokenLimitLayer<C> {
    /// A layer charging `limiter`.
    pub fn new(limiter: Arc<TokenLimiter<C>>) -> Self {
        Self { limiter }
    }
}

impl<S, C: Clock> Layer<S> for TokenLimitLayer<C> {
    type Service = TokenLimit<S, C>;

    fn layer(&self, inner: S) -> Self::Service {
        TokenLimit {
            inner,
            limiter: Arc::clone(&self.limiter),
        }
    }
}

/// The service produced by [`TokenLimitLayer`].
pub struct TokenLimit<S, C: Clock> {
    inner: S,
    limiter: Arc<TokenLimiter<C>>,
}

impl<S: Clone, C: Clock> Clone for TokenLimit<S, C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            limiter: Arc::clone(&self.limiter),
        }
    }
}

fn json(status: StatusCode, body: serde_json::Value) -> Response<Body> {
    let mut r = Response::new(Body::from(body.to_string()));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

/// Counts streamed tokens and reports them when the stream ends or is dropped.
struct Meter<C: Clock + 'static> {
    limiter: Arc<TokenLimiter<C>>,
    ticket: Option<TokenTicket>,
    tokens: u64,
    pending: Vec<u8>,
}

impl<C: Clock + 'static> Meter<C> {
    /// Counts complete `data:` events in `chunk`, except `[DONE]`.
    fn feed(&mut self, chunk: &[u8]) {
        self.pending.extend_from_slice(chunk);
        while let Some(end) = self.pending.windows(2).position(|w| w == b"\n\n") {
            let event: Vec<u8> = self.pending.drain(..end + 2).collect();
            let text = String::from_utf8_lossy(&event);
            if let Some(data) = text.trim().strip_prefix("data:")
                && data.trim() != "[DONE]"
            {
                self.tokens += 1;
            }
        }
    }
}

impl<C: Clock + 'static> Drop for Meter<C> {
    fn drop(&mut self) {
        if let Some(t) = self.ticket.take() {
            self.limiter.complete(t, self.tokens);
        }
    }
}

impl<S, C> Service<Request<Body>> for TokenLimit<S, C>
where
    S: Service<Request<Body>, Response = Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    C: Clock + 'static,
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
        let limiter = Arc::clone(&self.limiter);
        Box::pin(async move {
            let tenant = caller_key(&request);
            let (parts, body) = request.into_parts();
            let Ok(bytes) = axum::body::to_bytes(body, MAX_BODY).await else {
                return Ok(json(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    serde_json::json!({ "error": "body too large" }),
                ));
            };
            let Ok(req) = serde_json::from_slice::<CompletionRequest>(&bytes) else {
                return Ok(json(
                    StatusCode::BAD_REQUEST,
                    serde_json::json!({ "error": "expected JSON with model and prompt_tokens" }),
                ));
            };
            let mut ask = TokenRequest::new(&tenant, &req.model, req.prompt_tokens);
            ask.max_output = req.max_tokens;
            let decision = match limiter.admit(&ask) {
                Ok(d) => d,
                Err(e) => {
                    return Ok(json(
                        StatusCode::BAD_REQUEST,
                        serde_json::json!({ "error": e.to_string() }),
                    ));
                }
            };
            let Some(ticket) = decision.ticket else {
                let mut r = json(
                    StatusCode::TOO_MANY_REQUESTS,
                    serde_json::json!({
                        "error": "token_limit",
                        "debt": decision.debt,
                        "retry_after_ms": decision.retry_after.map(|d| d.as_millis() as u64),
                    }),
                );
                if let Some(wait) = decision.retry_after {
                    let secs = wait.as_millis().div_ceil(1_000).max(1) as u64;
                    r.headers_mut()
                        .insert(header::RETRY_AFTER, HeaderValue::from(secs));
                }
                r.headers_mut().insert(
                    HeaderName::from_static(DEBT_HEADER),
                    HeaderValue::from(decision.debt),
                );
                return Ok(r);
            };
            let request = Request::from_parts(parts, Body::from(bytes));
            let response = inner.call(request).await?;
            let (mut parts, body) = response.into_parts();
            parts.headers.insert(
                HeaderName::from_static(CHARGED_HEADER),
                HeaderValue::from_str(&format!(
                    "input={}, output={}",
                    decision.input_units, decision.output_units
                ))
                .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
            let mut meter = Meter {
                limiter,
                ticket: Some(ticket),
                tokens: 0,
                pending: Vec::new(),
            };
            let counted = body.into_data_stream().map(move |chunk| {
                if let Ok(bytes) = &chunk {
                    meter.feed(bytes);
                }
                chunk
            });
            Ok(Response::from_parts(parts, Body::from_stream(counted)))
        })
    }
}

/// How the mock model streams.
#[derive(Debug, Clone, Copy)]
pub struct MockModel {
    /// Delay before the first token.
    pub first_token: Duration,
    /// Delay between tokens.
    pub per_token: Duration,
}

/// `POST /v1/completions` on a mock model that streams tokens as server-sent events.
pub fn mock_router(model: MockModel) -> Router {
    Router::new()
        .route("/v1/completions", post(complete))
        .with_state(model)
}

/// A small deterministic hash, so a mock answer's random length depends only on the
/// request (replays give the same lengths).
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

async fn complete(State(model): State<MockModel>, body: Bytes) -> Response<Body> {
    let Ok(req) = serde_json::from_slice::<CompletionRequest>(&body) else {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "bad request" }),
        );
    };
    let max = req.max_tokens.unwrap_or(256).max(1);
    let tokens = req
        .completion_tokens
        .unwrap_or_else(|| 1 + mix(req.prompt_tokens ^ (body.len() as u64)) % max)
        .min(max);
    // Each token is due at a fixed time from the start, so timer rounding never adds up
    // over a long answer.
    let t0 = tokio::time::Instant::now();
    let stream = futures_util::stream::unfold(0u64, move |i| async move {
        if i > tokens {
            return None;
        }
        let due = model.first_token + model.per_token * u32::try_from(i).unwrap_or(u32::MAX);
        tokio::time::sleep_until(t0 + due).await;
        let event = if i == tokens {
            "data: [DONE]\n\n".to_owned()
        } else {
            format!("data: {{\"index\":{i},\"text\":\"tok\"}}\n\n")
        };
        Some((Ok::<_, Infallible>(Bytes::from(event)), i + 1))
    });
    let mut r = Response::new(Body::from_stream(stream));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratelab::tokens::{Accounting, ModelPolicy, TokenPolicy};
    use ratelab::{ManualClock, RateLimitConfig};
    use tower::ServiceExt;

    fn app(accounting: Accounting) -> (Router, Arc<TokenLimiter<Arc<ManualClock>>>) {
        let policy = TokenPolicy::new(
            RateLimitConfig::per_minute(10_000).unwrap(),
            RateLimitConfig::per_minute(100).unwrap(),
            accounting,
        )
        .model("m", ModelPolicy::new(1, 1, 80, 20));
        let limiter = Arc::new(TokenLimiter::new(policy, Arc::new(ManualClock::new())));
        let mock = MockModel {
            first_token: Duration::from_millis(5),
            per_token: Duration::from_millis(1),
        };
        (
            mock_router(mock).layer(TokenLimitLayer::new(Arc::clone(&limiter))),
            limiter,
        )
    }

    fn post(body: &str) -> Request<Body> {
        Request::post("/v1/completions")
            .header("x-api-key", "alice")
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    async fn read(r: Response<Body>) -> String {
        String::from_utf8(
            axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn the_meter_counts_events_split_across_chunks() {
        let policy = TokenPolicy::new(
            RateLimitConfig::per_minute(10).unwrap(),
            RateLimitConfig::per_minute(10).unwrap(),
            Accounting::EstimateOnly,
        );
        let mut m = Meter {
            limiter: Arc::new(TokenLimiter::new(policy, Arc::new(ManualClock::new()))),
            ticket: None,
            tokens: 0,
            pending: Vec::new(),
        };
        m.feed(b"data: a\n\nda");
        m.feed(b"ta: b\n");
        m.feed(b"\ndata: [DONE]\n\n");
        assert_eq!(m.tokens, 2);
    }

    // Tokio's paused clock: the mock's delays pass instantly.
    #[tokio::test(start_paused = true)]
    async fn streamed_tokens_are_counted_and_reconciled() {
        let (app, limiter) = app(Accounting::AdmitThenReconcile);
        let r = app
            .oneshot(post(
                r#"{"model":"m","prompt_tokens":40,"completion_tokens":70}"#,
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers()[CHARGED_HEADER], "input=40, output=20");
        let text = read(r).await;
        assert_eq!(text.matches("data: {").count(), 70);
        assert!(text.ends_with("data: [DONE]\n\n"));
        // Charged 20 up front, 70 produced: 50 more, so 100 - 70 left.
        assert_eq!(limiter.balance("alice").output, 30);
    }

    #[tokio::test(start_paused = true)]
    async fn debt_turns_into_429_with_retry_after() {
        let (app, limiter) = app(Accounting::AdmitThenReconcile);
        // Two answers stream at once, each charged the 20-token estimate up front.
        let body = r#"{"model":"m","prompt_tokens":1,"completion_tokens":80}"#;
        let a = app.clone().oneshot(post(body)).await.unwrap();
        let b = app.clone().oneshot(post(body)).await.unwrap();
        read(a).await;
        read(b).await;
        assert!(limiter.balance("alice").output < 0);
        let r = app
            .oneshot(post(r#"{"model":"m","prompt_tokens":1}"#))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(r.headers().contains_key(header::RETRY_AFTER));
        assert_eq!(r.headers()[DEBT_HEADER], "60");
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_leaves_pays_for_what_it_received() {
        let (app, limiter) = app(Accounting::AdmitThenReconcile);
        let r = app
            .oneshot(post(
                r#"{"model":"m","prompt_tokens":1,"completion_tokens":80}"#,
            ))
            .await
            .unwrap();
        let mut stream = r.into_body().into_data_stream();
        for _ in 0..30 {
            stream.next().await;
        }
        drop(stream);
        // 30 tokens received, 20 charged up front: 10 more.
        assert_eq!(limiter.balance("alice").output, 70);
    }

    #[tokio::test]
    async fn bad_requests_and_unknown_models_are_400() {
        let (app, _) = app(Accounting::ReserveMax);
        let r = app.clone().oneshot(post("not json")).await.unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = app
            .oneshot(post(r#"{"model":"x","prompt_tokens":1}"#))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }
}
