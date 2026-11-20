//! Token limits for LLM traffic, for the browser: a live [`TokenDemo`] (one limiter you
//! drive request by request) and [`TokenView`] (an hour of synthetic traffic through
//! every accounting mode and the oracle, from [`sim::tokens`]).

use ratelab::tokens::{
    Accounting, ModelPolicy, TokenLimiter, TokenPolicy, TokenRequest, TokenTicket,
};
use ratelab::{ManualClock, RateLimitConfig};
use sim::tokens::{
    Streaming, SynthModel, measure, oracle_requests, run, synthetic, wrongly_blocked_pct,
};
use std::sync::Arc;
use std::time::Duration;
use wasm_bindgen::prelude::*;

/// The one model the browser uses.
const MODEL: &str = "model";
/// Input budget: large enough never to matter here.
const INPUT_PER_MIN: u64 = 1_000_000_000;

fn policy(
    accounting: Accounting,
    output_per_min: u64,
    max_output: u64,
    prior: u64,
) -> Result<TokenPolicy, String> {
    let input = RateLimitConfig::per_minute(INPUT_PER_MIN).map_err(|e| e.to_string())?;
    let output = RateLimitConfig::per_minute(output_per_min).map_err(|e| e.to_string())?;
    Ok(TokenPolicy::new(input, output, accounting)
        .model(MODEL, ModelPolicy::new(1, 1, max_output.max(1), prior)))
}

/// One tenant's limiter, driven by hand: admit, let time pass, complete.
#[wasm_bindgen]
pub struct TokenDemo {
    limiter: TokenLimiter<Arc<ManualClock>>,
    clock: Arc<ManualClock>,
    tickets: Vec<Option<TokenTicket>>,
    limit: u64,
}

#[wasm_bindgen]
impl TokenDemo {
    /// A limiter with `accounting` (`estimate_only`, `reserve_max`,
    /// `admit_then_reconcile`), an output budget per minute, a maximum answer and a
    /// starting estimate. Throws on bad settings.
    #[wasm_bindgen(constructor)]
    pub fn new(
        accounting: &str,
        output_per_min: u32,
        max_output: u32,
        prior: u32,
    ) -> Result<TokenDemo, JsError> {
        let a: Accounting = accounting
            .parse()
            .map_err(|e: ratelab::tokens::TokenError| JsError::new(&e.to_string()))?;
        let p = policy(
            a,
            u64::from(output_per_min),
            u64::from(max_output),
            u64::from(prior),
        )
        .map_err(|e| JsError::new(&e))?;
        let clock = Arc::new(ManualClock::new());
        Ok(TokenDemo {
            limiter: TokenLimiter::new(p, Arc::clone(&clock)),
            clock,
            tickets: Vec::new(),
            limit: u64::from(output_per_min),
        })
    }

    /// Moves time forward.
    pub fn advance(&self, ms: f64) {
        self.clock
            .advance(Duration::from_secs_f64((ms.max(0.0)) / 1_000.0));
    }

    /// Admits a request; returns its ticket number, or -1 if refused.
    pub fn admit(&mut self, input_tokens: u32) -> i32 {
        let req = TokenRequest::new("tenant", MODEL, u64::from(input_tokens));
        match self.limiter.admit(&req).ok().and_then(|d| d.ticket) {
            Some(t) => {
                self.tickets.push(Some(t));
                i32::try_from(self.tickets.len() - 1).unwrap_or(-1)
            }
            None => -1,
        }
    }

    /// Output units the ticket was charged at admission (0 for an unknown ticket).
    pub fn charged(&self, ticket: i32) -> u32 {
        usize::try_from(ticket)
            .ok()
            .and_then(|i| self.tickets.get(i))
            .and_then(Option::as_ref)
            .map_or(0, |t| u32::try_from(t.charged_output()).unwrap_or(u32::MAX))
    }

    /// Finishes a ticket that produced `tokens`; returns the units charged now
    /// (negative: a refund).
    pub fn complete(&mut self, ticket: i32, tokens: u32) -> i32 {
        let Some(t) = usize::try_from(ticket)
            .ok()
            .and_then(|i| self.tickets.get_mut(i))
            .and_then(Option::take)
        else {
            return 0;
        };
        i32::try_from(self.limiter.complete(t, u64::from(tokens))).unwrap_or(0)
    }

    /// Output units left (negative: owed).
    pub fn balance(&self) -> f64 {
        self.limiter.balance("tenant").output as f64
    }

    /// The budget per minute.
    pub fn limit(&self) -> u32 {
        u32::try_from(self.limit).unwrap_or(u32::MAX)
    }

    /// The current estimate of an answer's length.
    pub fn estimate(&self) -> u32 {
        u32::try_from(self.limiter.estimate(MODEL)).unwrap_or(u32::MAX)
    }
}

/// Synthetic traffic through every accounting mode and the oracle.
#[wasm_bindgen]
pub struct TokenView {
    names: Vec<String>,
    blocked: Vec<f64>,
    wrongly: Vec<f64>,
    overspend: Vec<f64>,
    worst: Vec<f64>,
    debts: Vec<f64>,
    recovery: Vec<f64>,
    balance_t: Vec<Vec<f64>>,
    balance_v: Vec<Vec<f64>>,
    oracle_blocked: f64,
    requests: u32,
}

#[wasm_bindgen]
impl TokenView {
    /// `tenants` (Zipf-skewed) send `rate` requests per second for `secs`; answers
    /// average `output_mean` tokens (heavy-tailed), capped at `max_output`; each tenant
    /// may receive `output_per_min` units a minute; estimates start at `prior`.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        tenants: u32,
        rate: u32,
        secs: u32,
        output_mean: u32,
        max_output: u32,
        output_per_min: u32,
        prior: u32,
        seed: u32,
    ) -> Result<TokenView, JsError> {
        if u64::from(rate) * u64::from(secs) > 500_000 {
            return Err(JsError::new(
                "too many requests: lower the rate or the length",
            ));
        }
        let model = [SynthModel {
            share_pct: 1,
            input_mean: 500,
            output_mean: u64::from(output_mean.max(1)),
            max_output: u64::from(max_output.max(1)),
        }];
        let reqs = synthetic(
            u64::from(seed),
            Duration::from_secs(u64::from(secs)),
            u64::from(rate),
            tenants,
            &model,
        );
        let stream = Streaming {
            first_token: Duration::from_millis(200),
            per_token: Duration::from_millis(25),
        };
        let duration = Duration::from_secs(u64::from(secs));
        let models = [MODEL];
        let mk = |a| {
            policy(
                a,
                u64::from(output_per_min),
                u64::from(max_output),
                u64::from(prior),
            )
            .map_err(|e| JsError::new(&e))
        };
        let oracle = run(
            &mk(Accounting::ReserveMax)?,
            &models,
            &oracle_requests(&reqs),
            stream,
            None,
        );
        let oracle_blocked = oracle.outcomes.iter().filter(|o| !o.admitted).count() as f64 * 100.0
            / reqs.len().max(1) as f64;
        let mut v = TokenView {
            names: Vec::new(),
            blocked: Vec::new(),
            wrongly: Vec::new(),
            overspend: Vec::new(),
            worst: Vec::new(),
            debts: Vec::new(),
            recovery: Vec::new(),
            balance_t: Vec::new(),
            balance_v: Vec::new(),
            oracle_blocked,
            requests: u32::try_from(reqs.len()).unwrap_or(u32::MAX),
        };
        for a in Accounting::ALL {
            let p = mk(a)?;
            let r = run(&p, &models, &reqs, stream, Some(0));
            let m = measure(&p, &models, &reqs, &r, duration);
            v.names.push(a.name().to_owned());
            v.blocked.push(m.blocked_pct);
            v.wrongly.push(wrongly_blocked_pct(&r, &oracle));
            v.overspend.push(m.overspend_pct);
            v.worst.push(m.worst_window);
            v.debts.push(m.debts as f64);
            v.recovery.push(m.recovery_mean_ms);
            // Thin the balance trace to at most ~2,000 points.
            let every = (r.balance.len() / 2_000).max(1);
            v.balance_t.push(
                r.balance
                    .iter()
                    .step_by(every)
                    .map(|(t, _)| t.0 as f64 / 1e9)
                    .collect(),
            );
            v.balance_v.push(
                r.balance
                    .iter()
                    .step_by(every)
                    .map(|&(_, b)| b as f64)
                    .collect(),
            );
        }
        Ok(v)
    }

    /// Accounting mode names, in row order.
    pub fn names(&self) -> Vec<String> {
        self.names.clone()
    }
    /// Refused, %.
    pub fn blocked(&self) -> Vec<f64> {
        self.blocked.clone()
    }
    /// Fewer admitted than the oracle, % of requests.
    pub fn wrongly(&self) -> Vec<f64> {
        self.wrongly.clone()
    }
    /// Output served beyond budgets, % of output served.
    pub fn overspend(&self) -> Vec<f64> {
        self.overspend.clone()
    }
    /// Most output one tenant got in one window, × the limit.
    pub fn worst(&self) -> Vec<f64> {
        self.worst.clone()
    }
    /// Times a tenant went into debt.
    pub fn debts(&self) -> Vec<f64> {
        self.debts.clone()
    }
    /// Mean time to repay a debt, ms.
    #[wasm_bindgen(js_name = recoveryMs)]
    pub fn recovery_ms(&self) -> Vec<f64> {
        self.recovery.clone()
    }
    /// The busiest tenant's balance times (s) under mode `i`.
    #[wasm_bindgen(js_name = balanceT)]
    pub fn balance_t(&self, i: usize) -> Vec<f64> {
        self.balance_t.get(i).cloned().unwrap_or_default()
    }
    /// The busiest tenant's balance values (units) under mode `i`.
    #[wasm_bindgen(js_name = balanceV)]
    pub fn balance_v(&self, i: usize) -> Vec<f64> {
        self.balance_v.get(i).cloned().unwrap_or_default()
    }
    /// What the oracle refused, %.
    #[wasm_bindgen(js_name = oracleBlocked)]
    pub fn oracle_blocked(&self) -> f64 {
        self.oracle_blocked
    }
    /// Requests in the run.
    pub fn requests(&self) -> u32 {
        self.requests
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_goes_into_debt_and_recovers() {
        let mut d = TokenDemo::new("admit_then_reconcile", 600, 1_000, 100).unwrap();
        let a = d.admit(10);
        let b = d.admit(10);
        assert_eq!(d.charged(a), 100);
        assert_eq!(d.complete(a, 400), 300);
        d.complete(b, 400);
        assert_eq!(d.balance(), -200.0);
        assert_eq!(d.admit(10), -1);
        d.advance(60_000.0);
        assert!(d.balance() > 0.0);
        assert!(d.admit(10) >= 0);
        assert_eq!(d.complete(99, 1), 0, "unknown tickets are ignored");
    }

    #[test]
    fn the_view_reports_every_mode() {
        let v = TokenView::run(10, 4, 600, 200, 1_024, 6_000, 256, 1).unwrap();
        assert_eq!(
            v.names(),
            ["estimate_only", "reserve_max", "admit_then_reconcile"]
        );
        assert_eq!(v.overspend()[1], 0.0);
        assert!(v.wrongly()[1] >= v.wrongly()[2]);
        assert!(!v.balance_t(2).is_empty());
        assert!(v.requests() > 0);
    }
}
