//! Token-aware limits for LLM traffic: estimate-only, reserve-max and
//! admit-then-reconcile on real and synthetic LLM requests.
//!
//! **Real requests** come from the Azure LLM inference trace (November 2023): every
//! request to two production services, code and conversation, with its true input and
//! output token counts. It is downloaded into memory, checked against pinned SHA-256s
//! and discarded after the run. The trace has no user ids, so requests are given to
//! tenants by a seeded Zipf draw (a few heavy tenants, many light ones); each seed is
//! a different draw.
//!
//! **Limits** are not picked by hand. The input limit is the smallest per-tenant
//! limit per minute at which an oracle (which knows every answer's length in advance)
//! blocks at most 1% of requests on input alone; the output limit is then the smallest
//! at which the oracle blocks at most [`TARGET_BLOCKED_PCT`].
//!
//! **Answers stream** at [`FIRST_TOKEN_MS`] to the first token and [`PER_TOKEN_MS`] per
//! token after it (an assumption: the trace records counts, not timings).
//!
//! The same requests also run through the server's real [`TokenLimitLayer`] and mock
//! model, in real time (sped up [`SPEEDUP`]×), and through the simulator with the same
//! speed-up, to check the simulator.

use crate::machine::Machine;
use crate::stats::Summary;
use crate::traces::{TraceFile, days_from_civil, fetch};
use anyhow::bail;
use ratelab::tokens::{Accounting, ModelPolicy, TokenLimiter, TokenPolicy};
use ratelab::{Nanos, RateLimitConfig};
use serde::{Deserialize, Serialize};
use server::llm::{MockModel, TokenLimitLayer, mock_router};
use sim::tokens::{
    DebtSpan, LlmRequest, Outcome, Streaming, SynthModel, TokenMetrics, TokenRun, measure,
    oracle_requests, run, synthetic, wrongly_blocked_pct, zipf_tenants,
};
use std::sync::Arc;
use std::time::Duration;

/// The two services in the trace, used as the two models.
pub const MODELS: [&str; 2] = ["code", "conv"];
/// Tenants the requests are spread over.
pub const TENANTS: u32 = 20;
/// The limit window.
pub const WINDOW_SECS: u64 = 60;
/// Share of requests the oracle may block at the chosen output limit, %.
pub const TARGET_BLOCKED_PCT: f64 = 5.0;
/// Share of requests the oracle may block on input alone, %.
pub const INPUT_BLOCKED_PCT: f64 = 1.0;
/// Starting estimate of an answer's length, tokens (then learned).
pub const PRIOR_OUTPUT: u64 = 256;
/// Time to the first token, ms.
pub const FIRST_TOKEN_MS: u64 = 200;
/// Time per further token, ms.
pub const PER_TOKEN_MS: u64 = 25;
/// How much faster than the trace the real-time run plays.
pub const SPEEDUP: u64 = 5;

/// Azure LLM inference trace 2023 (Splitwise, ISCA 2024), CC-BY 4.0.
pub const AZURE_CODE: TraceFile = TraceFile {
    url: "https://raw.githubusercontent.com/Azure/AzurePublicDataset/master/data/AzureLLMInferenceTrace_code.csv",
    sha256: "54e9a6d2a4bd06ba1e060304b900abbc74cbea53de96506e60fe5bb4f2277fb6",
};
/// See [`AZURE_CODE`].
pub const AZURE_CONV: TraceFile = TraceFile {
    url: "https://raw.githubusercontent.com/Azure/AzurePublicDataset/master/data/AzureLLMInferenceTrace_conv.csv",
    sha256: "2f1e5b666d4e3055fdbba98598ce2ec307767b9064e03e2fa46676dbcc7d0bf8",
};
/// Where the trace and its terms are described.
pub const AZURE_HOMEPAGE: &str =
    "https://github.com/Azure/AzurePublicDataset/blob/master/AzureLLMInferenceDataset2023.md";
/// The paper the trace asks to be cited.
pub const AZURE_CITATION: &str =
    "Patel et al., Splitwise: Efficient generative LLM inference using phase splitting, ISCA 2024";

/// How long, how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmSize {
    /// Tenant draws (seeds) per scenario.
    pub seeds: u64,
    /// Minutes of the trace replayed in real time (at [`SPEEDUP`]×).
    pub real_minutes: u64,
    /// Length of the synthetic scenario, seconds.
    pub synthetic_secs: u64,
}

impl LlmSize {
    /// The published sizes.
    pub fn full() -> Self {
        Self {
            seeds: 5,
            real_minutes: 5,
            synthetic_secs: 3_600,
        }
    }

    /// For checking the pipeline.
    pub fn quick() -> Self {
        Self {
            seeds: 1,
            real_minutes: 1,
            synthetic_secs: 600,
        }
    }
}

/// Unix nanoseconds for `2023-11-16 18:15:46.6805900`.
pub fn azure_time(s: &str) -> Option<i64> {
    let (date, time) = s.trim().split_once(' ')?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':');
    let (h, mi, sec): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
    );
    let digits: String = frac.chars().take(9).collect();
    let ns: i64 = format!("{digits:0<9}").parse().ok()?;
    Some(((days_from_civil(y, m, day) * 86_400 + h * 3_600 + mi * 60 + sec) * 1_000_000_000) + ns)
}

/// Parses one trace file as requests of `model`: (unix ns, input, output).
pub fn parse_azure(csv: &[u8], model: usize) -> anyhow::Result<Vec<(i64, usize, u64, u64)>> {
    let text = std::str::from_utf8(csv)?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() {
            continue;
        }
        let mut f = line.split(',');
        let (Some(t), Some(input), Some(output)) = (f.next(), f.next(), f.next()) else {
            bail!(
                "line {}: expected TIMESTAMP,ContextTokens,GeneratedTokens",
                i + 1
            );
        };
        let Some(at) = azure_time(t) else {
            bail!("line {}: bad time {t:?}", i + 1);
        };
        out.push((at, model, input.trim().parse()?, output.trim().parse()?));
    }
    Ok(out)
}

/// Requests from parsed rows: merged in time order, from zero, tenants drawn with `seed`.
pub fn requests(rows: &[(i64, usize, u64, u64)], seed: u64) -> Vec<LlmRequest> {
    let mut rows = rows.to_vec();
    rows.sort_by_key(|r| (r.0, r.1));
    let start = rows.first().map_or(0, |r| r.0);
    let owners = zipf_tenants(seed, rows.len(), TENANTS);
    rows.iter()
        .zip(owners)
        .map(|(&(at, model, input, output), tenant)| LlmRequest {
            at: Nanos(u64::try_from(at - start).unwrap_or(0)),
            tenant,
            model,
            input: input.max(1),
            output,
            max_output: None,
        })
        .collect()
}

/// A model's longest answer allowed: the smallest power of two that fits every answer
/// seen for it, so no answer in the trace is cut short.
pub fn max_outputs(reqs: &[LlmRequest]) -> [u64; 2] {
    let mut m = [1u64; 2];
    for r in reqs {
        if r.model < 2 {
            m[r.model] = m[r.model].max(r.output);
        }
    }
    m.map(|x| x.next_power_of_two())
}

/// The policy for one accounting mode.
pub fn policy(
    accounting: Accounting,
    input: u64,
    output: u64,
    max_out: [u64; 2],
    window: Duration,
) -> TokenPolicy {
    let cfg = |limit: u64| {
        RateLimitConfig::new(limit.max(1), window).expect("limit and window are non-zero")
    };
    TokenPolicy::new(cfg(input), cfg(output), accounting)
        .model(MODELS[0], ModelPolicy::new(1, 1, max_out[0], PRIOR_OUTPUT))
        .model(MODELS[1], ModelPolicy::new(1, 1, max_out[1], PRIOR_OUTPUT))
}

/// Streaming speed at `speedup`×.
pub fn streaming(speedup: u64) -> Streaming {
    Streaming {
        first_token: Duration::from_millis(FIRST_TOKEN_MS) / speedup.max(1) as u32,
        per_token: Duration::from_millis(PER_TOKEN_MS) / speedup.max(1) as u32,
    }
}

fn oracle_blocked(reqs: &[LlmRequest], input: u64, output: u64, max_out: [u64; 2]) -> f64 {
    let p = policy(
        Accounting::ReserveMax,
        input,
        output,
        max_out,
        Duration::from_secs(WINDOW_SECS),
    );
    let oracle = oracle_requests(reqs);
    let r = run(&p, &MODELS, &oracle, streaming(1), None);
    let blocked = r.outcomes.iter().filter(|o| !o.admitted).count();
    blocked as f64 * 100.0 / reqs.len().max(1) as f64
}

/// Smallest `x` in `1..=hi` with `f(x) <= target` (`f` falls as `x` grows).
fn smallest(mut hi: u64, target: f64, f: impl Fn(u64) -> f64) -> u64 {
    let mut lo = 1;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if f(mid) <= target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

/// The input and output limits per tenant per minute (see the module docs).
pub fn choose_limits(reqs: &[LlmRequest], max_out: [u64; 2]) -> (u64, u64) {
    let total_in: u64 = reqs.iter().map(|r| r.input).sum();
    let total_out: u64 = reqs.iter().map(|r| r.output).sum::<u64>() + max_out.iter().sum::<u64>();
    let input = smallest(total_in.max(1), INPUT_BLOCKED_PCT, |l| {
        oracle_blocked(reqs, l, total_out.max(1), max_out)
    });
    let output = smallest(total_out.max(1), TARGET_BLOCKED_PCT, |l| {
        oracle_blocked(reqs, input, l, max_out)
    });
    (input, output)
}

/// One accounting mode in one scenario, over seeds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    /// `estimate_only`, `reserve_max` or `admit_then_reconcile`.
    pub accounting: String,
    /// Refused, % of requests.
    pub blocked_pct: Summary,
    /// How many fewer requests were admitted than by the oracle, % of requests.
    pub wrongly_blocked_pct: Summary,
    /// Output served beyond the tenants' budgets, % of output served.
    pub overspend_pct: Summary,
    /// Most output a tenant got in one window, × the limit.
    pub worst_window: Summary,
    /// Times a tenant went into debt.
    pub debts: Summary,
    /// Mean time to repay a debt, ms.
    pub recovery_mean_ms: Summary,
    /// p99 time to repay a debt, ms.
    pub recovery_p99_ms: Summary,
}

fn row(name: &str, runs: &[(TokenMetrics, f64)]) -> Row {
    let s =
        |f: fn(&(TokenMetrics, f64)) -> f64| Summary::of(&runs.iter().map(f).collect::<Vec<_>>());
    Row {
        accounting: name.to_owned(),
        blocked_pct: s(|r| r.0.blocked_pct),
        wrongly_blocked_pct: s(|r| r.1),
        overspend_pct: s(|r| r.0.overspend_pct),
        worst_window: s(|r| r.0.worst_window),
        debts: s(|r| r.0.debts as f64),
        recovery_mean_ms: s(|r| r.0.recovery_mean_ms),
        recovery_p99_ms: s(|r| r.0.recovery_p99_ms),
    }
}

/// One scenario's results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    /// `azure_2023` or `synthetic`.
    pub name: String,
    /// One sentence.
    pub description: String,
    /// Requests.
    pub requests: u64,
    /// Seconds from first to last request.
    pub span_secs: f64,
    /// Input units per tenant per minute.
    pub input_limit: u64,
    /// Output units per tenant per minute.
    pub output_limit: u64,
    /// Longest answer allowed, per model.
    pub max_output: Vec<(String, u64)>,
    /// Mean answer length, per model.
    pub mean_output: Vec<(String, f64)>,
    /// What the oracle blocked, %.
    pub oracle_blocked_pct: Summary,
    /// One row per accounting mode.
    pub rows: Vec<Row>,
    /// The busiest tenant's output balance over the first ten minutes, seed 1:
    /// `(accounting, [(seconds, units)])`.
    pub balance: Vec<(String, Vec<(f64, i64)>)>,
}

fn mean_outputs(reqs: &[LlmRequest]) -> Vec<(String, f64)> {
    MODELS
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let xs: Vec<f64> = reqs
                .iter()
                .filter(|r| r.model == i)
                .map(|r| r.output as f64)
                .collect();
            (
                (*m).to_owned(),
                xs.iter().sum::<f64>() / xs.len().max(1) as f64,
            )
        })
        .collect()
}

/// Runs every mode on `make(seed)` for each seed, with limits chosen on seed 1.
pub fn scenario(
    name: &str,
    description: &str,
    seeds: u64,
    make: impl Fn(u64) -> Vec<LlmRequest>,
    step: &impl Fn(&str),
) -> Scenario {
    let first = make(1);
    let max_out = max_outputs(&first);
    step(&format!("{name}: choosing limits"));
    let (input, output) = choose_limits(&first, max_out);
    let window = Duration::from_secs(WINDOW_SECS);
    let mut per_mode: Vec<Vec<(TokenMetrics, f64)>> = vec![Vec::new(); Accounting::ALL.len()];
    let mut oracle_blocked = Vec::new();
    let mut balance = Vec::new();
    let mut span = 0.0;
    let mut n = 0;
    for seed in 1..=seeds {
        step(&format!("{name}: seed {seed}"));
        let reqs = if seed == 1 { first.clone() } else { make(seed) };
        let duration = Duration::from_nanos(reqs.last().map_or(0, |r| r.at.0));
        span = duration.as_secs_f64();
        n = reqs.len() as u64;
        let op = policy(Accounting::ReserveMax, input, output, max_out, window);
        let oracle = run(&op, &MODELS, &oracle_requests(&reqs), streaming(1), None);
        oracle_blocked.push(
            oracle.outcomes.iter().filter(|o| !o.admitted).count() as f64 * 100.0
                / reqs.len().max(1) as f64,
        );
        for (k, a) in Accounting::ALL.into_iter().enumerate() {
            let p = policy(a, input, output, max_out, window);
            let r = run(&p, &MODELS, &reqs, streaming(1), (seed == 1).then_some(0));
            let m = measure(&p, &MODELS, &reqs, &r, duration);
            per_mode[k].push((m, wrongly_blocked_pct(&r, &oracle)));
            if seed == 1 {
                let ten_min = 600_000_000_000;
                balance.push((
                    a.name().to_owned(),
                    r.balance
                        .iter()
                        .filter(|(t, _)| t.0 <= ten_min)
                        .map(|&(t, b)| (t.0 as f64 / 1e9, b))
                        .collect(),
                ));
            }
        }
    }
    Scenario {
        name: name.to_owned(),
        description: description.to_owned(),
        requests: n,
        span_secs: span,
        input_limit: input,
        output_limit: output,
        max_output: MODELS
            .iter()
            .zip(max_out)
            .map(|(m, x)| ((*m).to_owned(), x))
            .collect(),
        mean_output: mean_outputs(&first),
        oracle_blocked_pct: Summary::of(&oracle_blocked),
        rows: Accounting::ALL
            .iter()
            .zip(&per_mode)
            .map(|(a, runs)| row(a.name(), runs))
            .collect(),
        balance,
    }
}

/// Synthetic requests shaped like the trace's two services, with heavier tails.
pub fn synthetic_requests(seed: u64, secs: u64, like: &[LlmRequest]) -> Vec<LlmRequest> {
    let means = mean_outputs(like);
    let input_mean = |i: usize| {
        let xs: Vec<u64> = like
            .iter()
            .filter(|r| r.model == i)
            .map(|r| r.input)
            .collect();
        (xs.iter().sum::<u64>() / xs.len().max(1) as u64).max(1)
    };
    let max_out = max_outputs(like);
    let share = |i: usize| like.iter().filter(|r| r.model == i).count() as u32;
    let rate = (like.len() as f64 / like.last().map_or(1.0, |r| r.at.0 as f64 / 1e9).max(1.0))
        .round() as u64;
    let models: Vec<SynthModel> = (0..2)
        .map(|i| SynthModel {
            share_pct: share(i).max(1),
            input_mean: input_mean(i),
            output_mean: (means[i].1.round() as u64).max(1),
            max_output: max_out[i],
        })
        .collect();
    synthetic(
        seed,
        Duration::from_secs(secs),
        rate.max(1),
        TENANTS,
        &models,
    )
}

// ---------- Real time, through the server's layer ----------

/// One mode on the real layer against the simulator, same requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealRow {
    /// Accounting mode.
    pub accounting: String,
    /// Refused, %: real, simulated.
    pub blocked_pct: (f64, f64),
    /// Overspend, %: real, simulated.
    pub overspend_pct: (f64, f64),
    /// Worst window × limit: real, simulated.
    pub worst_window: (f64, f64),
    /// Debts: real, simulated.
    pub debts: (u64, u64),
}

/// The real-time check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealCheck {
    /// Speed-up.
    pub speedup: u64,
    /// Minutes of the trace replayed.
    pub trace_minutes: u64,
    /// Requests replayed.
    pub requests: u64,
    /// One row per mode.
    pub rows: Vec<RealRow>,
}

/// `requests` at `speedup`× (times divided).
pub fn speed_up(reqs: &[LlmRequest], speedup: u64) -> Vec<LlmRequest> {
    reqs.iter()
        .map(|r| LlmRequest {
            at: Nanos(r.at.0 / speedup.max(1)),
            ..*r
        })
        .collect()
}

/// Tokio's clock as a [`ratelab::Clock`]: real time normally, and exact virtual time
/// when Tokio's clock is paused (in tests), so the limiter and the mock model agree.
#[derive(Debug, Clone, Copy)]
pub struct TokioClock(tokio::time::Instant);

impl ratelab::Clock for TokioClock {
    fn now(&self) -> Nanos {
        Nanos(u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX))
    }
}

/// Runs `reqs` through the real layer and mock model in real time.
pub async fn real_run(p: &TokenPolicy, reqs: &[LlmRequest], s: Streaming) -> TokenRun {
    use tower::ServiceExt;
    let limiter = Arc::new(TokenLimiter::new(
        p.clone(),
        TokioClock(tokio::time::Instant::now()),
    ));
    let app = mock_router(MockModel {
        first_token: s.first_token,
        per_token: s.per_token,
    })
    .layer(TokenLimitLayer::new(Arc::clone(&limiter)));
    let start = tokio::time::Instant::now();

    // Debt is seen by sampling every tenant's balance until the run is over and every
    // debt has been repaid.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let limiter = Arc::clone(&limiter);
        let stop = Arc::clone(&stop);
        let tenants: Vec<u32> = (0..TENANTS).collect();
        tokio::spawn(async move {
            let mut open: std::collections::BTreeMap<u32, DebtSpan> =
                std::collections::BTreeMap::new();
            let mut done = Vec::new();
            let mut tick = tokio::time::interval(Duration::from_millis(5));
            loop {
                tick.tick().await;
                let now = Nanos(start.elapsed().as_nanos() as u64);
                for &t in &tenants {
                    let b = limiter.balance(&format!("t{t}")).output;
                    if b < 0 {
                        let e = open.entry(t).or_insert(DebtSpan {
                            tenant: t,
                            from: now,
                            to: now,
                            peak: 0,
                        });
                        e.to = now;
                        e.peak = e.peak.max(b.unsigned_abs());
                    } else if let Some(d) = open.remove(&t) {
                        done.push(DebtSpan { to: now, ..d });
                    }
                }
                if stop.load(std::sync::atomic::Ordering::SeqCst) && open.is_empty() {
                    break done;
                }
            }
        })
    };

    let mut handles = Vec::with_capacity(reqs.len());
    for r in reqs {
        tokio::time::sleep_until(start + Duration::from_nanos(r.at.0)).await;
        let app = app.clone();
        let model = MODELS[r.model.min(1)];
        let max_tokens = p.model_policy(model).map_or(r.output, |m| m.max_output);
        let body = serde_json::json!({
            "model": model,
            "prompt_tokens": r.input,
            "max_tokens": max_tokens,
            "completion_tokens": r.output,
        })
        .to_string();
        let tenant = format!("t{}", r.tenant);
        handles.push(tokio::spawn(async move {
            let request = axum::http::Request::post("/v1/completions")
                .header("x-api-key", tenant)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body))
                .unwrap_or_default();
            let Ok(response) = app.oneshot(request).await;
            if response.status() != axum::http::StatusCode::OK {
                return Outcome {
                    admitted: false,
                    done_at: None,
                };
            }
            let _ = axum::body::to_bytes(response.into_body(), usize::MAX).await;
            Outcome {
                admitted: true,
                done_at: Some(Nanos(start.elapsed().as_nanos() as u64)),
            }
        }));
    }
    let mut outcomes = Vec::with_capacity(handles.len());
    for h in handles {
        outcomes.push(h.await.unwrap_or(Outcome {
            admitted: false,
            done_at: None,
        }));
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let debts = sampler.await.unwrap_or_default();
    TokenRun {
        outcomes,
        debts,
        balance: Vec::new(),
    }
}

/// The first `minutes` of `reqs` at [`SPEEDUP`]×, real against simulated, every mode.
pub fn real_check(
    reqs: &[LlmRequest],
    minutes: u64,
    step: &impl Fn(&str),
) -> anyhow::Result<RealCheck> {
    let cut = minutes * 60_000_000_000;
    let slice: Vec<LlmRequest> = reqs.iter().filter(|r| r.at.0 < cut).copied().collect();
    let max_out = max_outputs(reqs);
    let (input, output) = choose_limits(&slice, max_out);
    let fast = speed_up(&slice, SPEEDUP);
    let window = Duration::from_secs(WINDOW_SECS) / SPEEDUP as u32;
    let s = streaming(SPEEDUP);
    let duration = Duration::from_nanos(cut / SPEEDUP);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let mut rows = Vec::new();
    for a in Accounting::ALL {
        step(&format!("real time: {}", a.name()));
        let p = policy(a, input, output, max_out, window);
        let real = rt.block_on(real_run(&p, &fast, s));
        let simulated = run(&p, &MODELS, &fast, s, None);
        let mr = measure(&p, &MODELS, &fast, &real, duration);
        let ms = measure(&p, &MODELS, &fast, &simulated, duration);
        rows.push(RealRow {
            accounting: a.name().to_owned(),
            blocked_pct: (mr.blocked_pct, ms.blocked_pct),
            overspend_pct: (mr.overspend_pct, ms.overspend_pct),
            worst_window: (mr.worst_window, ms.worst_window),
            debts: (mr.debts, ms.debts),
        });
    }
    Ok(RealCheck {
        speedup: SPEEDUP,
        trace_minutes: minutes,
        requests: fast.len() as u64,
        rows,
    })
}

/// `results/llm.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResults {
    /// File format version.
    pub schema: u32,
    /// Where it ran.
    pub machine: Machine,
    /// Sizes.
    pub size: LlmSize,
    /// Trace files and checksums.
    pub files: Vec<(String, String)>,
    /// Where the trace is described.
    pub homepage: String,
    /// The paper to cite.
    pub citation: String,
    /// Tenants.
    pub tenants: u32,
    /// Window, seconds.
    pub window_secs: u64,
    /// Oracle blocking target for the output limit, %.
    pub target_blocked_pct: f64,
    /// Starting estimate, tokens.
    pub prior_output: u64,
    /// Streaming: first token, per token (ms).
    pub streaming_ms: (u64, u64),
    /// The scenarios.
    pub scenarios: Vec<Scenario>,
    /// Real layer against the simulator.
    pub real: Option<RealCheck>,
}

/// Downloads the trace (into memory) and parses it.
pub fn fetch_azure(step: &impl Fn(&str)) -> anyhow::Result<Vec<(i64, usize, u64, u64)>> {
    let mut rows = Vec::new();
    for (model, file) in [AZURE_CODE, AZURE_CONV].iter().enumerate() {
        step(&format!("downloading {}", file.url));
        let blob = fetch(file)?;
        rows.extend(parse_azure(&blob, model)?);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn azure_times_parse_to_the_nanosecond() {
        let a = azure_time("2023-11-16 18:15:46.6805900").unwrap();
        let b = azure_time("2023-11-16 18:15:50.9951690").unwrap();
        assert_eq!(b - a, 4_314_579_000);
        assert_eq!(
            azure_time("2023-11-16 00:00:00.0").unwrap() % 86_400_000_000_000,
            0
        );
        assert!(azure_time("nonsense").is_none());
    }

    #[test]
    fn parses_the_trace_format() {
        let csv = b"TIMESTAMP,ContextTokens,GeneratedTokens\n2023-11-16 18:15:46.6805900,374,44\n2023-11-16 18:15:47.0000000,396,109\n";
        let rows = parse_azure(csv, 1).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].1, rows[0].2, rows[0].3), (1, 374, 44));
        assert!(parse_azure(b"h\nbad line\n", 0).is_err());
        let reqs = requests(&rows, 1);
        assert_eq!(reqs[0].at, Nanos(0));
        assert_eq!(max_outputs(&reqs), [1, 128]);
    }

    fn small() -> Vec<LlmRequest> {
        let m = [
            SynthModel {
                share_pct: 30,
                input_mean: 2_000,
                output_mean: 30,
                max_output: 512,
            },
            SynthModel {
                share_pct: 70,
                input_mean: 1_000,
                output_mean: 200,
                max_output: 2_048,
            },
        ];
        synthetic(4, Duration::from_secs(900), 4, TENANTS, &m)
    }

    #[test]
    fn chosen_limits_hit_the_oracle_target() {
        let reqs = small();
        let max_out = max_outputs(&reqs);
        let (input, output) = choose_limits(&reqs, max_out);
        assert!(oracle_blocked(&reqs, input, output, max_out) <= TARGET_BLOCKED_PCT);
        assert!(oracle_blocked(&reqs, input, output - 1, max_out) > TARGET_BLOCKED_PCT);
    }

    #[test]
    fn a_scenario_has_every_mode_and_a_balance_trace() {
        let s = scenario(
            "t",
            "test",
            2,
            |seed| {
                let mut r = small();
                let o = zipf_tenants(seed, r.len(), TENANTS);
                for (x, t) in r.iter_mut().zip(o) {
                    x.tenant = t;
                }
                r
            },
            &|_| {},
        );
        assert_eq!(s.rows.len(), 3);
        assert_eq!(s.balance.len(), 3);
        let res = s
            .rows
            .iter()
            .find(|r| r.accounting == "reserve_max")
            .unwrap();
        assert_eq!(res.overspend_pct.mean, 0.0);
        assert!(res.wrongly_blocked_pct.mean > 0.0);
    }

    #[test]
    fn on_exact_time_the_real_layer_makes_the_simulators_decisions() {
        // Tokio's paused clock: the server, the mock model and the limiter share exact
        // virtual time, so every decision must match the simulator's, one by one.
        let m = [
            SynthModel {
                share_pct: 30,
                input_mean: 2_000,
                output_mean: 30,
                max_output: 512,
            },
            SynthModel {
                share_pct: 70,
                input_mean: 1_000,
                output_mean: 200,
                max_output: 1_024,
            },
        ];
        let reqs = synthetic(4, Duration::from_secs(60), 6, TENANTS, &m);
        let max_out = max_outputs(&reqs);
        let (input, output) = choose_limits(&reqs, max_out);
        for a in Accounting::ALL {
            let p = policy(a, input, output, max_out, Duration::from_secs(60));
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .start_paused(true)
                .build()
                .unwrap();
            let real = rt.block_on(real_run(&p, &reqs, streaming(1)));
            let simulated = run(&p, &MODELS, &reqs, streaming(1), None);
            let admitted = |r: &TokenRun| r.outcomes.iter().map(|o| o.admitted).collect::<Vec<_>>();
            assert_eq!(admitted(&real), admitted(&simulated), "{}", a.name());
            assert!(admitted(&real).contains(&false), "the limit was reached");
        }
    }
}
