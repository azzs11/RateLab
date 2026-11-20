//! Token-aware limits for LLM traffic.
//!
//! An LLM request is not one unit of work: it costs its input (prompt) tokens, known
//! when it arrives, and its output tokens, known only when the response has finished
//! streaming. [`TokenLimiter`] keeps two budgets per tenant, one for input and one for
//! output, each a token bucket refilled continuously, and charges each request in
//! *weighted units*: tokens times a per-model weight, so a large model can cost more
//! per token than a small one (like a price list).
//!
//! The output charge is the hard part, and [`Accounting`] decides how to make it:
//!
//! - [`Accounting::EstimateOnly`]: charge an estimate when the request is admitted and
//!   never correct it. Simple, and wrong both ways: long answers overspend, short
//!   ones waste budget.
//! - [`Accounting::ReserveMax`]: reserve the request's maximum output (`max_tokens`)
//!   and refund what was not used. Never overspends, but holds budget that most
//!   requests never use, so it blocks requests that would have fit.
//! - [`Accounting::AdmitThenReconcile`]: charge the estimate, then charge (or refund)
//!   the difference once the true count is known. A response longer than expected
//!   puts the tenant into *debt*: its output balance goes below zero and new requests
//!   are refused until the refill has paid it back.
//!
//! Estimates are learned per model: a moving average of the true output counts seen
//! so far, starting from the model's `expected_output`. Time is injected through a
//! [`Clock`], as everywhere in this crate, and all arithmetic is exact integers.
//!
//! ```
//! use ratelab::tokens::{Accounting, ModelPolicy, TokenLimiter, TokenPolicy, TokenRequest};
//! use ratelab::{ManualClock, RateLimitConfig};
//!
//! // 1,000 input and 500 output units per minute per tenant; one model at weight 1.
//! let policy = TokenPolicy::new(
//!     RateLimitConfig::per_minute(1_000)?,
//!     RateLimitConfig::per_minute(500)?,
//!     Accounting::AdmitThenReconcile,
//! )
//! .model("small", ModelPolicy::new(1, 1, 400, 100));
//! let clock = ManualClock::new();
//! let limiter = TokenLimiter::new(policy, &clock);
//!
//! let d = limiter.admit(&TokenRequest::new("alice", "small", 200))?;
//! assert!(d.allowed);
//! // The answer was 300 tokens, not the 100 estimated: 200 more are charged.
//! limiter.complete(d.ticket.unwrap(), 300);
//! assert_eq!(limiter.balance("alice").output, 200);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use crate::algorithms::{ceil_div_nanos, with_state};
use crate::{Clock, Nanos, RateLimitConfig};
use dashmap::DashMap;
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;
use thiserror::Error;

/// How the output charge is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Accounting {
    /// Charge an estimate at admission; never correct it.
    EstimateOnly,
    /// Reserve the maximum output at admission; refund the unused part at the end.
    ReserveMax,
    /// Charge an estimate at admission; charge or refund the difference at the end,
    /// carrying any shortfall as debt.
    AdmitThenReconcile,
}

impl Accounting {
    /// Every mode, in a fixed order.
    pub const ALL: [Accounting; 3] = [
        Self::EstimateOnly,
        Self::ReserveMax,
        Self::AdmitThenReconcile,
    ];

    /// Short name: `estimate_only`, `reserve_max` or `admit_then_reconcile`.
    ///
    /// ```
    /// use ratelab::tokens::Accounting;
    /// assert_eq!(Accounting::ReserveMax.name(), "reserve_max");
    /// assert_eq!("admit_then_reconcile".parse::<Accounting>(), Ok(Accounting::AdmitThenReconcile));
    /// ```
    pub fn name(&self) -> &'static str {
        match self {
            Self::EstimateOnly => "estimate_only",
            Self::ReserveMax => "reserve_max",
            Self::AdmitThenReconcile => "admit_then_reconcile",
        }
    }
}

impl std::str::FromStr for Accounting {
    type Err = TokenError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|a| a.name() == s)
            .ok_or_else(|| TokenError::UnknownAccounting(s.to_owned()))
    }
}

/// Errors from the token limiter.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TokenError {
    /// The request named a model the policy does not know.
    #[error("unknown model {0:?}")]
    UnknownModel(String),
    /// An accounting mode name was not recognised.
    #[error("unknown accounting {0:?} (estimate_only, reserve_max or admit_then_reconcile)")]
    UnknownAccounting(String),
}

/// What one model costs and how long its answers may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPolicy {
    /// Units charged per input token.
    pub input_weight: u64,
    /// Units charged per output token.
    pub output_weight: u64,
    /// Longest answer allowed, in tokens (what a request's `max_tokens` defaults to).
    pub max_output: u64,
    /// The first estimate of an answer's length, before any has been seen.
    pub expected_output: u64,
}

impl ModelPolicy {
    /// A model with these weights, maximum and starting estimate.
    ///
    /// ```
    /// use ratelab::tokens::ModelPolicy;
    /// let large = ModelPolicy::new(3, 6, 4_096, 300);
    /// assert_eq!(large.output_weight, 6);
    /// ```
    pub fn new(
        input_weight: u64,
        output_weight: u64,
        max_output: u64,
        expected_output: u64,
    ) -> Self {
        Self {
            input_weight,
            output_weight,
            max_output,
            expected_output: expected_output.min(max_output),
        }
    }
}

/// Limits, accounting mode and models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPolicy {
    input: RateLimitConfig,
    output: RateLimitConfig,
    accounting: Accounting,
    models: BTreeMap<String, ModelPolicy>,
}

impl TokenPolicy {
    /// Per-tenant input and output budgets, in weighted units per window.
    ///
    /// ```
    /// use ratelab::tokens::{Accounting, TokenPolicy};
    /// use ratelab::RateLimitConfig;
    /// let p = TokenPolicy::new(
    ///     RateLimitConfig::per_minute(10_000)?,
    ///     RateLimitConfig::per_minute(2_000)?,
    ///     Accounting::ReserveMax,
    /// );
    /// assert_eq!(p.accounting(), Accounting::ReserveMax);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn new(input: RateLimitConfig, output: RateLimitConfig, accounting: Accounting) -> Self {
        Self {
            input,
            output,
            accounting,
            models: BTreeMap::new(),
        }
    }

    /// Adds (or replaces) a model.
    ///
    /// ```
    /// use ratelab::tokens::{Accounting, ModelPolicy, TokenPolicy};
    /// use ratelab::RateLimitConfig;
    /// let p = TokenPolicy::new(RateLimitConfig::per_minute(100)?, RateLimitConfig::per_minute(100)?, Accounting::EstimateOnly)
    ///     .model("small", ModelPolicy::new(1, 1, 256, 64));
    /// assert!(p.model_policy("small").is_some());
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn model(mut self, name: impl Into<String>, policy: ModelPolicy) -> Self {
        self.models.insert(name.into(), policy);
        self
    }

    /// The same policy with another accounting mode.
    ///
    /// ```
    /// # use ratelab::tokens::{Accounting, TokenPolicy};
    /// # use ratelab::RateLimitConfig;
    /// let p = TokenPolicy::new(RateLimitConfig::per_minute(1)?, RateLimitConfig::per_minute(1)?, Accounting::EstimateOnly);
    /// assert_eq!(p.with_accounting(Accounting::ReserveMax).accounting(), Accounting::ReserveMax);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn with_accounting(mut self, accounting: Accounting) -> Self {
        self.accounting = accounting;
        self
    }

    /// The accounting mode.
    ///
    /// ```
    /// # use ratelab::tokens::{Accounting, TokenPolicy};
    /// # use ratelab::RateLimitConfig;
    /// # let p = TokenPolicy::new(RateLimitConfig::per_minute(1)?, RateLimitConfig::per_minute(1)?, Accounting::EstimateOnly);
    /// assert_eq!(p.accounting(), Accounting::EstimateOnly);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn accounting(&self) -> Accounting {
        self.accounting
    }

    /// The input budget.
    ///
    /// ```
    /// # use ratelab::tokens::{Accounting, TokenPolicy};
    /// # use ratelab::RateLimitConfig;
    /// # let p = TokenPolicy::new(RateLimitConfig::per_minute(7)?, RateLimitConfig::per_minute(1)?, Accounting::EstimateOnly);
    /// assert_eq!(p.input().limit(), 7);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn input(&self) -> RateLimitConfig {
        self.input
    }

    /// The output budget.
    ///
    /// ```
    /// # use ratelab::tokens::{Accounting, TokenPolicy};
    /// # use ratelab::RateLimitConfig;
    /// # let p = TokenPolicy::new(RateLimitConfig::per_minute(7)?, RateLimitConfig::per_minute(9)?, Accounting::EstimateOnly);
    /// assert_eq!(p.output().limit(), 9);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn output(&self) -> RateLimitConfig {
        self.output
    }

    /// A model's policy.
    ///
    /// ```
    /// # use ratelab::tokens::{Accounting, TokenPolicy};
    /// # use ratelab::RateLimitConfig;
    /// # let p = TokenPolicy::new(RateLimitConfig::per_minute(7)?, RateLimitConfig::per_minute(9)?, Accounting::EstimateOnly);
    /// assert!(p.model_policy("nope").is_none());
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn model_policy(&self, name: &str) -> Option<&ModelPolicy> {
        self.models.get(name)
    }

    /// Model names, sorted.
    ///
    /// ```
    /// # use ratelab::tokens::{Accounting, ModelPolicy, TokenPolicy};
    /// # use ratelab::RateLimitConfig;
    /// # let p = TokenPolicy::new(RateLimitConfig::per_minute(7)?, RateLimitConfig::per_minute(9)?, Accounting::EstimateOnly)
    /// #     .model("b", ModelPolicy::new(1, 1, 10, 5)).model("a", ModelPolicy::new(1, 1, 10, 5));
    /// assert_eq!(p.models().collect::<Vec<_>>(), ["a", "b"]);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn models(&self) -> impl Iterator<Item = &str> {
        self.models.keys().map(String::as_str)
    }
}

/// One request as the limiter sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRequest<'a> {
    /// Who pays.
    pub tenant: &'a str,
    /// Which model.
    pub model: &'a str,
    /// Prompt tokens.
    pub input_tokens: u64,
    /// The request's `max_tokens`; the model's maximum when `None` (and never above it).
    pub max_output: Option<u64>,
}

impl<'a> TokenRequest<'a> {
    /// A request with the model's default maximum output.
    ///
    /// ```
    /// use ratelab::tokens::TokenRequest;
    /// let r = TokenRequest::new("alice", "small", 120).max_output(64);
    /// assert_eq!(r.max_output, Some(64));
    /// ```
    pub fn new(tenant: &'a str, model: &'a str, input_tokens: u64) -> Self {
        Self {
            tenant,
            model,
            input_tokens,
            max_output: None,
        }
    }

    /// Sets `max_tokens`.
    ///
    /// ```
    /// # use ratelab::tokens::TokenRequest;
    /// assert_eq!(TokenRequest::new("a", "m", 1).max_output(5).max_output, Some(5));
    /// ```
    pub fn max_output(mut self, max_output: u64) -> Self {
        self.max_output = Some(max_output);
        self
    }
}

/// Proof that a request was admitted; give it back with [`TokenLimiter::complete`].
#[derive(Debug, PartialEq, Eq)]
#[must_use = "an admitted request must be completed so its output can be accounted for"]
pub struct TokenTicket {
    tenant: String,
    model: String,
    /// Output units charged at admission.
    charged: u64,
    max_output: u64,
}

impl TokenTicket {
    /// Output units charged at admission.
    ///
    /// ```
    /// # use ratelab::tokens::*;
    /// # use ratelab::{ManualClock, RateLimitConfig};
    /// # let p = TokenPolicy::new(RateLimitConfig::per_minute(1_000)?, RateLimitConfig::per_minute(1_000)?, Accounting::ReserveMax)
    /// #     .model("m", ModelPolicy::new(1, 2, 100, 10));
    /// # let clock = ManualClock::new();
    /// # let l = TokenLimiter::new(p, &clock);
    /// let t = l.admit(&TokenRequest::new("a", "m", 1))?.ticket.unwrap();
    /// assert_eq!(t.charged_output(), 200); // 100 tokens reserved at weight 2
    /// # l.complete(t, 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn charged_output(&self) -> u64 {
        self.charged
    }

    /// The most output tokens this request may produce.
    ///
    /// ```
    /// # use ratelab::tokens::*;
    /// # use ratelab::{ManualClock, RateLimitConfig};
    /// # let p = TokenPolicy::new(RateLimitConfig::per_minute(1_000)?, RateLimitConfig::per_minute(1_000)?, Accounting::ReserveMax)
    /// #     .model("m", ModelPolicy::new(1, 1, 100, 10));
    /// # let clock = ManualClock::new();
    /// # let l = TokenLimiter::new(p, &clock);
    /// let t = l.admit(&TokenRequest::new("a", "m", 1).max_output(30))?.ticket.unwrap();
    /// assert_eq!(t.max_output(), 30);
    /// # l.complete(t, 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn max_output(&self) -> u64 {
        self.max_output
    }
}

/// The answer to [`TokenLimiter::admit`].
#[derive(Debug, PartialEq, Eq)]
pub struct TokenDecision {
    /// Whether the request may go to the model.
    pub allowed: bool,
    /// Present when allowed.
    pub ticket: Option<TokenTicket>,
    /// Input units charged (0 if refused).
    pub input_units: u64,
    /// Output units charged now (0 if refused).
    pub output_units: u64,
    /// When refused: how long until the same request would fit (`None` if it never can,
    /// because it costs more than a whole window's budget).
    pub retry_after: Option<Duration>,
    /// Output units the tenant owes (0 unless in debt).
    pub debt: u64,
}

/// A tenant's balances in whole weighted units; negative output means debt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenBalance {
    /// Input units available.
    pub input: i64,
    /// Output units available (negative: owed).
    pub output: i64,
}

/// A token bucket whose level may go below zero (debt). Levels are units × window
/// nanoseconds, so refill (`limit` per window) is exact integer arithmetic.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    level: i128,
    last: Nanos,
}

#[derive(Debug, Clone, Copy)]
struct Scale {
    limit: i128,
    window: i128,
}

impl Scale {
    fn of(c: RateLimitConfig) -> Self {
        Self {
            limit: i128::from(c.limit()),
            window: i128::from(c.window_nanos()),
        }
    }

    fn capacity(&self) -> i128 {
        self.limit * self.window
    }

    fn full(&self, now: Nanos) -> Bucket {
        Bucket {
            level: self.capacity(),
            last: now,
        }
    }

    fn refill(&self, b: &mut Bucket, now: Nanos) {
        if now > b.last {
            let elapsed = i128::from(now.0 - b.last.0);
            b.level = (b.level + elapsed * self.limit).min(self.capacity());
            b.last = now;
        }
    }

    /// Time until the bucket holds `units`; `None` if it never can.
    fn wait(&self, b: &Bucket, units: u64) -> Option<Duration> {
        let need = i128::from(units) * self.window;
        if need > self.capacity() {
            return None;
        }
        let deficit = need - b.level;
        if deficit <= 0 {
            return Some(Duration::ZERO);
        }
        Some(ceil_div_nanos(
            deficit.unsigned_abs(),
            self.limit.unsigned_abs(),
        ))
    }

    fn whole(&self, b: &Bucket) -> i64 {
        i64::try_from(b.level.div_euclid(self.window)).unwrap_or(if b.level < 0 {
            i64::MIN
        } else {
            i64::MAX
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct Tenant {
    input: Bucket,
    output: Bucket,
}

/// Learned output estimates per model, in 1/256 tokens (an exponential moving average
/// with weight 1/8 on each new answer).
const EST_SHIFT: u32 = 8;

/// Token budgets per tenant. Thread-safe; share it in an `Arc`.
#[derive(Debug)]
pub struct TokenLimiter<C: Clock> {
    policy: TokenPolicy,
    clock: C,
    input: Scale,
    output: Scale,
    tenants: DashMap<String, Tenant>,
    estimates: Mutex<BTreeMap<String, u64>>,
}

impl<C: Clock> TokenLimiter<C> {
    /// A limiter enforcing `policy` with time from `clock`.
    ///
    /// ```
    /// # use ratelab::tokens::*;
    /// # use ratelab::{ManualClock, RateLimitConfig};
    /// let p = TokenPolicy::new(RateLimitConfig::per_minute(10)?, RateLimitConfig::per_minute(10)?, Accounting::EstimateOnly);
    /// let clock = ManualClock::new();
    /// let l = TokenLimiter::new(p, &clock);
    /// assert_eq!(l.policy().accounting(), Accounting::EstimateOnly);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn new(policy: TokenPolicy, clock: C) -> Self {
        let estimates = policy
            .models
            .iter()
            .map(|(name, m)| (name.clone(), m.expected_output << EST_SHIFT))
            .collect();
        Self {
            input: Scale::of(policy.input),
            output: Scale::of(policy.output),
            policy,
            clock,
            tenants: DashMap::new(),
            estimates: Mutex::new(estimates),
        }
    }

    /// The policy.
    ///
    /// ```
    /// # use ratelab::tokens::*;
    /// # use ratelab::{ManualClock, RateLimitConfig};
    /// # let p = TokenPolicy::new(RateLimitConfig::per_minute(10)?, RateLimitConfig::per_minute(20)?, Accounting::EstimateOnly);
    /// # let clock = ManualClock::new();
    /// let l = TokenLimiter::new(p, &clock);
    /// assert_eq!(l.policy().output().limit(), 20);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn policy(&self) -> &TokenPolicy {
        &self.policy
    }

    /// The current output estimate for `model`, in tokens (0 for an unknown model).
    ///
    /// ```
    /// # use ratelab::tokens::*;
    /// # use ratelab::{ManualClock, RateLimitConfig};
    /// # let p = TokenPolicy::new(RateLimitConfig::per_minute(10)?, RateLimitConfig::per_minute(20)?, Accounting::EstimateOnly)
    /// #     .model("m", ModelPolicy::new(1, 1, 500, 80));
    /// # let clock = ManualClock::new();
    /// let l = TokenLimiter::new(p, &clock);
    /// assert_eq!(l.estimate("m"), 80);
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn estimate(&self, model: &str) -> u64 {
        let e = self
            .estimates
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        e.get(model)
            .map_or(0, |v| (v + (1 << (EST_SHIFT - 1))) >> EST_SHIFT)
    }

    /// Decides one request and, if it is allowed, charges its input and the output
    /// [`Accounting`] calls for.
    ///
    /// ```
    /// # use ratelab::tokens::*;
    /// # use ratelab::{ManualClock, RateLimitConfig};
    /// let p = TokenPolicy::new(RateLimitConfig::per_minute(100)?, RateLimitConfig::per_minute(100)?, Accounting::ReserveMax)
    ///     .model("m", ModelPolicy::new(1, 1, 60, 10));
    /// let clock = ManualClock::new();
    /// let l = TokenLimiter::new(p, &clock);
    /// let first = l.admit(&TokenRequest::new("a", "m", 10))?;
    /// assert!(first.allowed); // reserves all 60 output tokens
    /// let second = l.admit(&TokenRequest::new("a", "m", 10))?;
    /// assert!(!second.allowed); // only 40 left, and it might need 60
    /// assert!(l.admit(&TokenRequest::new("a", "x", 1)).is_err());
    /// # l.complete(first.ticket.unwrap(), 5);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn admit(&self, req: &TokenRequest<'_>) -> Result<TokenDecision, TokenError> {
        let model = self
            .policy
            .model_policy(req.model)
            .ok_or_else(|| TokenError::UnknownModel(req.model.to_owned()))?;
        let max_output = req
            .max_output
            .unwrap_or(model.max_output)
            .min(model.max_output);
        let output_tokens = match self.policy.accounting {
            Accounting::ReserveMax => max_output,
            _ => self.estimate(req.model).min(max_output),
        };
        let input_units = req.input_tokens.saturating_mul(model.input_weight);
        let output_units = output_tokens.saturating_mul(model.output_weight);
        let now = self.clock.now();
        let (input, output) = (self.input, self.output);
        Ok(with_state(
            &self.tenants,
            req.tenant,
            || Tenant {
                input: input.full(now),
                output: output.full(now),
            },
            |t| {
                input.refill(&mut t.input, now);
                output.refill(&mut t.output, now);
                let need_in = i128::from(input_units) * input.window;
                let need_out = i128::from(output_units) * output.window;
                let debt = u64::try_from(-output.whole(&t.output)).unwrap_or(0);
                if t.input.level >= need_in && t.output.level >= need_out && t.output.level >= 0 {
                    t.input.level -= need_in;
                    t.output.level -= need_out;
                    TokenDecision {
                        allowed: true,
                        ticket: Some(TokenTicket {
                            tenant: req.tenant.to_owned(),
                            model: req.model.to_owned(),
                            charged: output_units,
                            max_output,
                        }),
                        input_units,
                        output_units,
                        retry_after: None,
                        debt,
                    }
                } else {
                    let retry_after = match (
                        input.wait(&t.input, input_units),
                        output.wait(&t.output, output_units),
                    ) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        _ => None,
                    };
                    TokenDecision {
                        allowed: false,
                        ticket: None,
                        input_units: 0,
                        output_units: 0,
                        retry_after,
                        debt,
                    }
                }
            },
        ))
    }

    /// Accounts for an admitted request that produced `output_tokens` (clamped to its
    /// maximum). Returns the output units charged now: positive means more was
    /// charged, negative means a refund.
    ///
    /// ```
    /// # use ratelab::tokens::*;
    /// # use ratelab::{ManualClock, RateLimitConfig};
    /// let p = TokenPolicy::new(RateLimitConfig::per_minute(1_000)?, RateLimitConfig::per_minute(100)?, Accounting::AdmitThenReconcile)
    ///     .model("m", ModelPolicy::new(1, 1, 500, 50));
    /// let clock = ManualClock::new();
    /// let l = TokenLimiter::new(p, &clock);
    /// let t = l.admit(&TokenRequest::new("a", "m", 1))?.ticket.unwrap();
    /// assert_eq!(l.complete(t, 250), 200); // 50 estimated, 250 produced
    /// assert_eq!(l.balance("a").output, -150); // in debt
    /// assert!(!l.admit(&TokenRequest::new("a", "m", 1))?.allowed);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn complete(&self, ticket: TokenTicket, output_tokens: u64) -> i64 {
        let Some(model) = self.policy.model_policy(&ticket.model) else {
            return 0;
        };
        let produced = output_tokens.min(ticket.max_output);
        {
            let mut e = self
                .estimates
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(est) = e.get_mut(&ticket.model) {
                let target = produced << EST_SHIFT;
                *est = if target >= *est {
                    *est + (target - *est) / 8
                } else {
                    *est - (*est - target) / 8
                };
            }
        }
        let actual_units = i128::from(produced.saturating_mul(model.output_weight));
        let charged = i128::from(ticket.charged);
        let delta = match self.policy.accounting {
            Accounting::EstimateOnly => 0,
            // Never more than reserved, so this is a refund (or nothing).
            Accounting::ReserveMax => (actual_units - charged).min(0),
            Accounting::AdmitThenReconcile => actual_units - charged,
        };
        if delta != 0 {
            let now = self.clock.now();
            let output = self.output;
            if let Some(mut t) = self.tenants.get_mut(&ticket.tenant) {
                output.refill(&mut t.output, now);
                t.output.level = (t.output.level - delta * output.window).min(output.capacity());
            }
        }
        i64::try_from(delta).unwrap_or(0)
    }

    /// A tenant's balances now (a full budget for a tenant never seen).
    ///
    /// ```
    /// # use ratelab::tokens::*;
    /// # use ratelab::{ManualClock, RateLimitConfig};
    /// # let p = TokenPolicy::new(RateLimitConfig::per_minute(10)?, RateLimitConfig::per_minute(20)?, Accounting::EstimateOnly);
    /// # let clock = ManualClock::new();
    /// let l = TokenLimiter::new(p, &clock);
    /// assert_eq!(l.balance("new"), TokenBalance { input: 10, output: 20 });
    /// # Ok::<(), ratelab::ConfigError>(())
    /// ```
    pub fn balance(&self, tenant: &str) -> TokenBalance {
        let now = self.clock.now();
        let (input, output) = (self.input, self.output);
        match self.tenants.get_mut(tenant) {
            Some(mut t) => {
                input.refill(&mut t.input, now);
                output.refill(&mut t.output, now);
                TokenBalance {
                    input: input.whole(&t.input),
                    output: output.whole(&t.output),
                }
            }
            None => TokenBalance {
                input: i64::try_from(input.limit).unwrap_or(i64::MAX),
                output: i64::try_from(output.limit).unwrap_or(i64::MAX),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManualClock;

    fn policy(accounting: Accounting) -> TokenPolicy {
        TokenPolicy::new(
            RateLimitConfig::per_minute(10_000).unwrap(),
            RateLimitConfig::per_minute(600).unwrap(),
            accounting,
        )
        .model("small", ModelPolicy::new(1, 1, 400, 100))
        .model("large", ModelPolicy::new(2, 4, 400, 100))
    }

    #[test]
    fn input_is_charged_exactly_with_the_model_weight() {
        let clock = ManualClock::new();
        let l = TokenLimiter::new(policy(Accounting::EstimateOnly), &clock);
        let d = l.admit(&TokenRequest::new("a", "large", 1_000)).unwrap();
        assert_eq!(d.input_units, 2_000);
        assert_eq!(d.output_units, 400); // estimate 100 at weight 4
        assert_eq!(l.balance("a").input, 8_000);
        l.complete(d.ticket.unwrap(), 100);
    }

    #[test]
    fn estimate_only_never_charges_the_difference() {
        let clock = ManualClock::new();
        let l = TokenLimiter::new(policy(Accounting::EstimateOnly), &clock);
        for _ in 0..3 {
            let d = l.admit(&TokenRequest::new("a", "small", 1)).unwrap();
            l.complete(d.ticket.unwrap(), 400);
        }
        // The estimate learns (100, then 138, then 170) but nothing is ever trued up:
        // 1,200 tokens produced against 408 charged.
        assert_eq!(l.balance("a").output, 600 - 408);
    }

    #[test]
    fn reserve_max_refunds_and_never_goes_into_debt() {
        let clock = ManualClock::new();
        let l = TokenLimiter::new(policy(Accounting::ReserveMax), &clock);
        let d = l.admit(&TokenRequest::new("a", "small", 1)).unwrap();
        assert_eq!(d.output_units, 400);
        assert_eq!(l.balance("a").output, 200);
        // A second request cannot reserve 400 while the first is streaming.
        assert!(
            !l.admit(&TokenRequest::new("a", "small", 1))
                .unwrap()
                .allowed
        );
        assert_eq!(l.complete(d.ticket.unwrap(), 50), -350);
        assert_eq!(l.balance("a").output, 550);
        assert!(
            l.admit(&TokenRequest::new("a", "small", 1))
                .unwrap()
                .allowed
        );
    }

    #[test]
    fn reconcile_carries_debt_until_the_refill_repays_it() {
        let clock = ManualClock::new();
        let l = TokenLimiter::new(policy(Accounting::AdmitThenReconcile), &clock);
        let a = l.admit(&TokenRequest::new("a", "small", 1)).unwrap();
        let b = l.admit(&TokenRequest::new("a", "small", 1)).unwrap();
        assert!(a.allowed && b.allowed);
        l.complete(a.ticket.unwrap(), 400);
        l.complete(b.ticket.unwrap(), 400);
        // Charged 100 + 100 up front, produced 800: 600 budget, 200 owed.
        assert_eq!(l.balance("a").output, -200);
        let refused = l.admit(&TokenRequest::new("a", "small", 1)).unwrap();
        assert!(!refused.allowed);
        assert_eq!(refused.debt, 200);
        // 600 per minute is 10 per second: the debt plus the next estimate takes time.
        let wait = refused.retry_after.unwrap();
        clock.advance(wait);
        assert!(
            l.admit(&TokenRequest::new("a", "small", 1))
                .unwrap()
                .allowed
        );
    }

    #[test]
    fn reconcile_refunds_short_answers() {
        let clock = ManualClock::new();
        let l = TokenLimiter::new(policy(Accounting::AdmitThenReconcile), &clock);
        let d = l.admit(&TokenRequest::new("a", "small", 1)).unwrap();
        assert_eq!(l.complete(d.ticket.unwrap(), 10), -90);
        assert_eq!(l.balance("a").output, 590);
    }

    #[test]
    fn estimates_follow_the_answers() {
        let clock = ManualClock::new();
        let l = TokenLimiter::new(policy(Accounting::AdmitThenReconcile), &clock);
        for _ in 0..60 {
            let d = l.admit(&TokenRequest::new("a", "small", 1)).unwrap();
            l.complete(d.ticket.unwrap(), 20);
            clock.advance(Duration::from_secs(10));
        }
        assert!(
            (20..=22).contains(&l.estimate("small")),
            "{}",
            l.estimate("small")
        );
        assert_eq!(l.estimate("large"), 100, "per model");
    }

    #[test]
    fn max_tokens_caps_the_charge_and_the_answer() {
        let clock = ManualClock::new();
        let l = TokenLimiter::new(policy(Accounting::ReserveMax), &clock);
        let d = l
            .admit(&TokenRequest::new("a", "small", 1).max_output(30))
            .unwrap();
        assert_eq!(d.output_units, 30);
        // It produced "50": only 30 are possible.
        assert_eq!(l.complete(d.ticket.unwrap(), 50), 0);
        let big = l
            .admit(&TokenRequest::new("b", "small", 1).max_output(10_000))
            .unwrap();
        assert_eq!(big.output_units, 400, "never above the model maximum");
        l.complete(big.ticket.unwrap(), 0);
    }

    #[test]
    fn a_request_larger_than_a_window_can_never_succeed() {
        let clock = ManualClock::new();
        let l = TokenLimiter::new(policy(Accounting::EstimateOnly), &clock);
        let d = l.admit(&TokenRequest::new("a", "small", 20_000)).unwrap();
        assert!(!d.allowed);
        assert_eq!(d.retry_after, None);
    }

    #[test]
    fn tenants_are_independent_and_shareable_across_threads() {
        let clock = std::sync::Arc::new(ManualClock::new());
        let l = std::sync::Arc::new(TokenLimiter::new(
            policy(Accounting::AdmitThenReconcile),
            clock,
        ));
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let l = std::sync::Arc::clone(&l);
                std::thread::spawn(move || {
                    let tenant = format!("t{i}");
                    let mut admitted = 0;
                    for _ in 0..10 {
                        let d = l.admit(&TokenRequest::new(&tenant, "small", 1)).unwrap();
                        if let Some(t) = d.ticket {
                            admitted += 1;
                            l.complete(t, 100);
                        }
                    }
                    admitted
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap(), 6, "600 per minute at 100 each");
        }
    }
}
