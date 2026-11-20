//! LLM traffic through a [`TokenLimiter`]: requests whose output streams back over time,
//! so the true cost is known only when the stream ends.
//!
//! Each admitted request finishes `first_token + output × per_token` after it arrives,
//! and is completed (reconciled) then. Between admission and completion the limiter
//! only has its estimate, which is the whole problem.
//!
//! ```
//! use drl_core::tokens::{Accounting, ModelPolicy, TokenPolicy};
//! use drl_core::RateLimitConfig;
//! use sim::tokens::{measure, run, synthetic, Streaming, SynthModel};
//! use std::time::Duration;
//!
//! let models = [SynthModel { share_pct: 100, input_mean: 300, output_mean: 150, max_output: 1_000 }];
//! let reqs = synthetic(1, Duration::from_secs(120), 4, 3, &models);
//! let policy = TokenPolicy::new(
//!     RateLimitConfig::per_minute(200_000)?,
//!     RateLimitConfig::per_minute(6_000)?,
//!     Accounting::EstimateOnly,
//! )
//! .model("m", ModelPolicy::new(1, 1, 1_000, 150));
//! let stream = Streaming { first_token: Duration::from_millis(200), per_token: Duration::from_millis(20) };
//! let r = run(&policy, &["m"], &reqs, stream, None);
//! let m = measure(&policy, &["m"], &reqs, &r, Duration::from_secs(120));
//! assert!(m.overspend_pct > 0.0); // estimates are never corrected
//! # Ok::<(), drl_core::ConfigError>(())
//! ```

use crate::SplitMix64;
use crate::scenarios::{exponential_q32, pareto2_q32, scale};
use drl_core::tokens::{TokenLimiter, TokenPolicy, TokenRequest, TokenTicket};
use drl_core::{ManualClock, Nanos};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;

/// One LLM request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmRequest {
    /// Arrival time.
    pub at: Nanos,
    /// Who sends it (the key is `"t{tenant}"`).
    pub tenant: u32,
    /// Index into the model list.
    pub model: usize,
    /// Prompt tokens.
    pub input: u64,
    /// Tokens the model will actually produce.
    pub output: u64,
    /// The request's `max_tokens` (`None`: the model's maximum).
    pub max_output: Option<u64>,
}

/// How fast answers stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Streaming {
    /// Time to the first token.
    pub first_token: Duration,
    /// Time per further token.
    pub per_token: Duration,
}

impl Streaming {
    /// When a request arriving at `at` with `output` tokens finishes.
    ///
    /// ```
    /// # use sim::tokens::Streaming;
    /// # use std::time::Duration;
    /// let s = Streaming { first_token: Duration::from_millis(100), per_token: Duration::from_millis(10) };
    /// assert_eq!(s.done(drl_core::Nanos(0), 5), drl_core::Nanos(150_000_000));
    /// ```
    pub fn done(&self, at: Nanos, output: u64) -> Nanos {
        let per = u64::try_from(self.per_token.as_nanos()).unwrap_or(u64::MAX);
        at.saturating_add(self.first_token)
            .saturating_add(Duration::from_nanos(per.saturating_mul(output)))
    }
}

/// A model in synthetic traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SynthModel {
    /// Share of requests, % (shares are relative; they need not add up to 100).
    pub share_pct: u32,
    /// Mean prompt tokens (exponential).
    pub input_mean: u64,
    /// Mean answer tokens (heavy-tailed: Pareto, shape 2), capped at `max_output`.
    pub output_mean: u64,
    /// The cap.
    pub max_output: u64,
}

/// Tenants for `n` requests, skewed like real API traffic: tenant `k` sends in
/// proportion to `1 / (k + 1)` (Zipf). Used to give anonymous traces owners.
///
/// ```
/// let t = sim::tokens::zipf_tenants(1, 10_000, 4);
/// let first = t.iter().filter(|&&x| x == 0).count();
/// let last = t.iter().filter(|&&x| x == 3).count();
/// assert!(first > 3 * last);
/// ```
pub fn zipf_tenants(seed: u64, n: usize, tenants: u32) -> Vec<u32> {
    let tenants = tenants.max(1);
    // Weights scaled to integers: 1/(k+1) × L, with L the lcm-free 720_720.
    let weights: Vec<u64> = (0..tenants).map(|k| 720_720 / u64::from(k + 1)).collect();
    let total: u64 = weights.iter().sum();
    let mut rng = SplitMix64::new(seed ^ 0x7E4A_A75E_C0DE_0001);
    (0..n)
        .map(|_| {
            let mut x = rng.below(total);
            let mut k = 0;
            while x >= weights[k] {
                x -= weights[k];
                k += 1;
            }
            k as u32
        })
        .collect()
}

/// Synthetic LLM traffic: Poisson arrivals at `rate_per_sec`, Zipf tenants, models
/// drawn by share, exponential prompts and heavy-tailed answers.
///
/// ```
/// use sim::tokens::{synthetic, SynthModel};
/// use std::time::Duration;
/// let m = [SynthModel { share_pct: 1, input_mean: 100, output_mean: 50, max_output: 400 }];
/// let r = synthetic(2, Duration::from_secs(60), 10, 5, &m);
/// assert!((500..700).contains(&r.len()));
/// assert!(r.iter().all(|x| x.output <= 400 && x.input >= 1));
/// ```
pub fn synthetic(
    seed: u64,
    duration: Duration,
    rate_per_sec: u64,
    tenants: u32,
    models: &[SynthModel],
) -> Vec<LlmRequest> {
    let end = Nanos::from_duration(duration).0;
    let gap = 1_000_000_000u64
        .checked_div(rate_per_sec)
        .unwrap_or(u64::MAX);
    let total_share: u64 = models
        .iter()
        .map(|m| u64::from(m.share_pct))
        .sum::<u64>()
        .max(1);
    let mut gaps = SplitMix64::new(seed);
    let mut sizes = SplitMix64::new(seed ^ 0x51DE_5EED_0000_0002);
    let mut pick = SplitMix64::new(seed ^ 0x51DE_5EED_0000_0003);
    let mut now = 0u64;
    let mut out = Vec::new();
    while !models.is_empty() {
        now = now.saturating_add(scale(exponential_q32(&mut gaps), gap).max(1));
        if now >= end {
            break;
        }
        let mut x = pick.below(total_share);
        let mut model = 0;
        while x >= u64::from(models[model].share_pct) {
            x -= u64::from(models[model].share_pct);
            model += 1;
        }
        let m = models[model];
        let input = scale(exponential_q32(&mut sizes), m.input_mean).max(1);
        let output = scale(pareto2_q32(&mut sizes), m.output_mean).clamp(1, m.max_output.max(1));
        out.push(LlmRequest {
            at: Nanos(now),
            tenant: 0,
            model,
            input,
            output,
            max_output: None,
        });
    }
    let owners = zipf_tenants(seed, out.len(), tenants);
    for (r, t) in out.iter_mut().zip(owners) {
        r.tenant = t;
    }
    out
}

/// What happened to one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    /// Admitted.
    pub admitted: bool,
    /// When its answer finished (admitted requests only).
    pub done_at: Option<Nanos>,
}

/// A period a tenant spent in debt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebtSpan {
    /// Tenant.
    pub tenant: u32,
    /// When it went into debt.
    pub from: Nanos,
    /// When the refill had paid it back.
    pub to: Nanos,
    /// The most it owed, in output units.
    pub peak: u64,
}

/// One run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRun {
    /// One per request, in order.
    pub outcomes: Vec<Outcome>,
    /// Every stretch of debt.
    pub debts: Vec<DebtSpan>,
    /// The watched tenant's output balance after every event that touched it.
    pub balance: Vec<(Nanos, i64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Event {
    Done(usize),
    Arrive(usize),
}

#[derive(Debug, Clone, Copy)]
struct OpenDebt {
    from: Nanos,
    until: Nanos,
    peak: u64,
}

/// Runs `requests` (sorted by time) through a limiter built from `policy`. `models[i]`
/// names the model of `model == i`. `watch` records that tenant's output balance.
pub fn run(
    policy: &TokenPolicy,
    models: &[&str],
    requests: &[LlmRequest],
    streaming: Streaming,
    watch: Option<u32>,
) -> TokenRun {
    let clock = Arc::new(ManualClock::new());
    let limiter = TokenLimiter::new(policy.clone(), Arc::clone(&clock));
    let window = u128::from(policy.output().window_nanos());
    let limit = u128::from(policy.output().limit()).max(1);
    let mut heap: BinaryHeap<Reverse<(u64, Event)>> = requests
        .iter()
        .enumerate()
        .map(|(i, r)| Reverse((r.at.0, Event::Arrive(i))))
        .collect();
    let mut tickets: Vec<Option<TokenTicket>> = (0..requests.len()).map(|_| None).collect();
    let mut outcomes = vec![
        Outcome {
            admitted: false,
            done_at: None
        };
        requests.len()
    ];
    let mut open: std::collections::BTreeMap<u32, OpenDebt> = std::collections::BTreeMap::new();
    let mut debts = Vec::new();
    let mut balance = Vec::new();
    let tenant_key = |t: u32| format!("t{t}");

    while let Some(Reverse((now, event))) = heap.pop() {
        clock.set(Nanos(now));
        // Close debts the refill has paid off by now.
        let paid: Vec<u32> = open
            .iter()
            .filter(|(_, d)| d.until.0 <= now)
            .map(|(t, _)| *t)
            .collect();
        for t in paid {
            if let Some(d) = open.remove(&t) {
                debts.push(DebtSpan {
                    tenant: t,
                    from: d.from,
                    to: d.until,
                    peak: d.peak,
                });
            }
        }
        let tenant = match event {
            Event::Arrive(i) => {
                let r = &requests[i];
                let key = tenant_key(r.tenant);
                let mut req =
                    TokenRequest::new(&key, models.get(r.model).copied().unwrap_or(""), r.input);
                req.max_output = r.max_output;
                if let Ok(d) = limiter.admit(&req)
                    && let Some(t) = d.ticket
                {
                    let done = streaming.done(r.at, r.output.min(t.max_output()));
                    tickets[i] = Some(t);
                    outcomes[i] = Outcome {
                        admitted: true,
                        done_at: Some(done),
                    };
                    heap.push(Reverse((done.0, Event::Done(i))));
                }
                r.tenant
            }
            Event::Done(i) => {
                let r = &requests[i];
                if let Some(t) = tickets[i].take() {
                    limiter.complete(t, r.output);
                }
                let b = limiter.balance(&tenant_key(r.tenant)).output;
                if b < 0 {
                    let owed = u128::from(b.unsigned_abs());
                    let wait = (owed * window).div_ceil(limit);
                    let until = Nanos(now.saturating_add(u64::try_from(wait).unwrap_or(u64::MAX)));
                    let e = open.entry(r.tenant).or_insert(OpenDebt {
                        from: Nanos(now),
                        until,
                        peak: 0,
                    });
                    e.until = until;
                    e.peak = e.peak.max(b.unsigned_abs());
                }
                r.tenant
            }
        };
        if watch == Some(tenant) {
            balance.push((Nanos(now), limiter.balance(&tenant_key(tenant)).output));
        }
    }
    debts.extend(open.into_iter().map(|(t, d)| DebtSpan {
        tenant: t,
        from: d.from,
        to: d.until,
        peak: d.peak,
    }));
    debts.sort_by_key(|d| (d.from, d.tenant));
    TokenRun {
        outcomes,
        debts,
        balance,
    }
}

/// What a run achieved.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenMetrics {
    /// Requests offered.
    pub requests: u64,
    /// Admitted.
    pub admitted: u64,
    /// Refused, % of requests.
    pub blocked_pct: f64,
    /// Output units actually produced for admitted requests.
    pub served_output: u64,
    /// Output units served beyond each tenant's budget for the run, as a % of all
    /// output served. A tenant's budget is what its bucket can ever release in the
    /// run: one window's limit to start with, plus the refill.
    pub overspend_pct: f64,
    /// The most output units any tenant received in one window-long span, as a
    /// multiple of the window's limit (units count when their answer finishes). A
    /// token bucket allows up to about 2: a full bucket plus a window of refill.
    pub worst_window: f64,
    /// Times a tenant went into debt.
    pub debts: u64,
    /// Mean time to pay a debt back, ms (0 without debts).
    pub recovery_mean_ms: f64,
    /// p99 time to pay a debt back, ms.
    pub recovery_p99_ms: f64,
    /// The most any tenant owed, output units.
    pub peak_debt: u64,
}

/// Measures a run of `requests` over `duration`.
pub fn measure(
    policy: &TokenPolicy,
    models: &[&str],
    requests: &[LlmRequest],
    run: &TokenRun,
    duration: Duration,
) -> TokenMetrics {
    let out = policy.output();
    let window = out.window().as_secs_f64();
    let budget = out.limit() as f64 * (1.0 + duration.as_secs_f64() / window.max(1e-9));
    let mut served: std::collections::BTreeMap<u32, u64> = std::collections::BTreeMap::new();
    let mut finished: std::collections::BTreeMap<u32, Vec<(u64, u64)>> =
        std::collections::BTreeMap::new();
    let mut admitted = 0u64;
    for (r, o) in requests.iter().zip(&run.outcomes) {
        if !o.admitted {
            continue;
        }
        admitted += 1;
        let Some(m) = models.get(r.model).and_then(|n| policy.model_policy(n)) else {
            continue;
        };
        let cap = r.max_output.unwrap_or(m.max_output).min(m.max_output);
        let units = r.output.min(cap) * m.output_weight;
        *served.entry(r.tenant).or_default() += units;
        if let Some(done) = o.done_at {
            finished.entry(r.tenant).or_default().push((done.0, units));
        }
    }
    let span = out.window_nanos();
    let mut worst = 0u64;
    for list in finished.values_mut() {
        list.sort_unstable();
        let (mut lo, mut sum) = (0usize, 0u64);
        for hi in 0..list.len() {
            sum += list[hi].1;
            while list[hi].0 - list[lo].0 >= span {
                sum -= list[lo].1;
                lo += 1;
            }
            worst = worst.max(sum);
        }
    }
    let total: u64 = served.values().sum();
    let excess: f64 = served.values().map(|&s| (s as f64 - budget).max(0.0)).sum();
    let mut rec: Vec<f64> = run
        .debts
        .iter()
        .map(|d| (d.to.0 - d.from.0) as f64 / 1e6)
        .collect();
    rec.sort_by(f64::total_cmp);
    let p99 = if rec.is_empty() {
        0.0
    } else {
        rec[((rec.len() as f64 * 0.99).ceil() as usize).clamp(1, rec.len()) - 1]
    };
    let n = requests.len() as u64;
    TokenMetrics {
        requests: n,
        admitted,
        blocked_pct: if n == 0 {
            0.0
        } else {
            (n - admitted) as f64 * 100.0 / n as f64
        },
        served_output: total,
        overspend_pct: if total == 0 {
            0.0
        } else {
            excess * 100.0 / total as f64
        },
        worst_window: worst as f64 / out.limit().max(1) as f64,
        debts: run.debts.len() as u64,
        recovery_mean_ms: if rec.is_empty() {
            0.0
        } else {
            rec.iter().sum::<f64>() / rec.len() as f64
        },
        recovery_p99_ms: p99,
        peak_debt: run.debts.iter().map(|d| d.peak).max().unwrap_or(0),
    }
}

/// Requests lost to guessing: how many fewer requests `run` admitted than `oracle`
/// (which knew every answer's length in advance), as a % of requests. Counted in
/// total rather than request by request, because one different decision changes
/// every later one.
///
/// ```
/// use sim::tokens::{wrongly_blocked_pct, Outcome, TokenRun};
/// let o = |admitted| Outcome { admitted, done_at: None };
/// let run = TokenRun { outcomes: vec![o(true), o(false), o(false), o(true)], debts: vec![], balance: vec![] };
/// let oracle = TokenRun { outcomes: vec![o(true), o(true), o(true), o(false)], debts: vec![], balance: vec![] };
/// assert_eq!(wrongly_blocked_pct(&run, &oracle), 25.0);
/// ```
pub fn wrongly_blocked_pct(run: &TokenRun, oracle: &TokenRun) -> f64 {
    let n = run.outcomes.len();
    if n == 0 {
        return 0.0;
    }
    let admitted = |r: &TokenRun| r.outcomes.iter().filter(|o| o.admitted).count();
    admitted(oracle).saturating_sub(admitted(run)) as f64 * 100.0 / n as f64
}

/// The oracle's view of `requests`: each request's `max_tokens` set to exactly what it
/// will produce, so reserve-max accounting charges the true cost up front.
///
/// ```
/// # use sim::tokens::{oracle_requests, LlmRequest};
/// let r = [LlmRequest { at: drl_core::Nanos(0), tenant: 0, model: 0, input: 5, output: 42, max_output: None }];
/// assert_eq!(oracle_requests(&r)[0].max_output, Some(42));
/// ```
pub fn oracle_requests(requests: &[LlmRequest]) -> Vec<LlmRequest> {
    requests
        .iter()
        .map(|r| LlmRequest {
            max_output: Some(r.max_output.map_or(r.output, |m| m.min(r.output))),
            ..*r
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use drl_core::RateLimitConfig;
    use drl_core::tokens::{Accounting, ModelPolicy};

    const MODELS: [&str; 1] = ["m"];

    fn policy(accounting: Accounting) -> TokenPolicy {
        TokenPolicy::new(
            RateLimitConfig::per_minute(1_000_000).unwrap(),
            RateLimitConfig::per_minute(3_000).unwrap(),
            accounting,
        )
        .model("m", ModelPolicy::new(1, 1, 2_000, 150))
    }

    fn stream() -> Streaming {
        Streaming {
            first_token: Duration::from_millis(200),
            per_token: Duration::from_millis(20),
        }
    }

    fn traffic(seed: u64) -> Vec<LlmRequest> {
        let m = [SynthModel {
            share_pct: 100,
            input_mean: 500,
            output_mean: 150,
            max_output: 2_000,
        }];
        synthetic(seed, Duration::from_secs(600), 2, 3, &m)
    }

    fn metrics(a: Accounting, reqs: &[LlmRequest]) -> (TokenMetrics, TokenRun) {
        let p = policy(a);
        let r = run(&p, &MODELS, reqs, stream(), Some(0));
        (measure(&p, &MODELS, reqs, &r, Duration::from_secs(600)), r)
    }

    #[test]
    fn the_oracle_never_overspends_and_never_goes_into_debt() {
        let reqs = oracle_requests(&traffic(1));
        let (m, r) = metrics(Accounting::ReserveMax, &reqs);
        assert_eq!(m.overspend_pct, 0.0);
        assert!(r.debts.is_empty());
        assert!(m.blocked_pct > 0.0, "the busiest tenant is over its limit");
    }

    #[test]
    fn estimate_only_overspends_reserve_max_blocks_reconcile_does_neither_much() {
        let reqs = traffic(2);
        let oracle = run(
            &policy(Accounting::ReserveMax),
            &MODELS,
            &oracle_requests(&reqs),
            stream(),
            None,
        );
        let (est, est_run) = metrics(Accounting::EstimateOnly, &reqs);
        let (res, res_run) = metrics(Accounting::ReserveMax, &reqs);
        let (rec, rec_run) = metrics(Accounting::AdmitThenReconcile, &reqs);
        assert!(
            est.overspend_pct > 0.0,
            "estimate-only {}",
            est.overspend_pct
        );
        assert_eq!(res.overspend_pct, 0.0);
        assert!(
            rec.overspend_pct < est.overspend_pct / 4.0,
            "reconcile {}",
            rec.overspend_pct
        );
        let wrong_res = wrongly_blocked_pct(&res_run, &oracle);
        let wrong_rec = wrongly_blocked_pct(&rec_run, &oracle);
        assert!(
            wrong_res > wrong_rec,
            "reserve {wrong_res} vs reconcile {wrong_rec}"
        );
        assert!(est_run.debts.is_empty() && res_run.debts.is_empty());
        assert!(rec.debts > 0 && rec.recovery_mean_ms > 0.0);
        assert!(!rec_run.balance.is_empty());
        assert!(rec_run.balance.iter().any(|&(_, b)| b < 0));
    }

    #[test]
    fn runs_are_deterministic() {
        let reqs = traffic(3);
        let p = policy(Accounting::AdmitThenReconcile);
        assert_eq!(
            run(&p, &MODELS, &reqs, stream(), Some(0)),
            run(&p, &MODELS, &reqs, stream(), Some(0))
        );
    }

    #[test]
    fn debts_close_when_the_refill_repays_them() {
        // One tenant, one huge answer: debt of 3,000 units at 50 per second = 60 s.
        let p = TokenPolicy::new(
            RateLimitConfig::per_minute(1_000_000).unwrap(),
            RateLimitConfig::per_minute(3_000).unwrap(),
            Accounting::AdmitThenReconcile,
        )
        .model("m", ModelPolicy::new(1, 1, 6_000, 0));
        let reqs = [LlmRequest {
            at: Nanos(0),
            tenant: 0,
            model: 0,
            input: 1,
            output: 6_000,
            max_output: None,
        }];
        let r = run(
            &p,
            &MODELS,
            &reqs,
            Streaming {
                first_token: Duration::ZERO,
                per_token: Duration::ZERO,
            },
            None,
        );
        assert_eq!(r.debts.len(), 1);
        assert_eq!(r.debts[0].peak, 3_000);
        assert_eq!(r.debts[0].to.0 - r.debts[0].from.0, 60_000_000_000);
    }

    #[test]
    fn zipf_tenants_cover_every_tenant() {
        let t = zipf_tenants(9, 5_000, 5);
        for k in 0..5 {
            assert!(t.contains(&k));
        }
    }
}
