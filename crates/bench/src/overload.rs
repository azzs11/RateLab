//! Overload: goodput and latency of a backend with limited capacity, with no limit, a
//! static concurrency limit, AIMD and Gradient2 in front of it.
//!
//! Each scenario runs twice per seed on the **same** requests (same arrival times,
//! same service times):
//!
//! - **simulated**: [`sim::overload`], a deterministic discrete-event model;
//! - **real**: the server's actual [`ConcurrencyLimitLayer`] and
//!   [`server::capacity`] backend on Tokio, driven in-process through the Axum router
//!   with real timers (no network, so the load generator measures the server and not
//!   the loopback interface).
//!
//! The backend does not cancel work that missed its deadline, so without a limit an
//! overload turns into congestion collapse. The static limit is set to the backend's
//! healthy worker count: the best static value, if the operator knows it and it never
//! changes.

use crate::machine::Machine;
use crate::stats::Summary;
use ratelab::Nanos;
use ratelab::concurrency::{AimdConfig, Concurrency, ConcurrencyLimiter, Gradient2Config};
use serde::{Deserialize, Serialize};
use server::capacity::{CapacityBackend, WORK_HEADER, work_router};
use server::concurrency::{ConcurrencyLimitLayer, LIMIT_HEADER};
use sim::overload::{self, OverloadConfig, OverloadMetrics, Request, Served, Status};
use std::sync::Arc;
use std::time::Duration;

/// Backend workers when healthy.
pub const WORKERS: usize = 16;
/// Mean service time, ms.
pub const SERVICE_MS: u64 = 20;
/// The server answers 504 after this, ms.
pub const DEADLINE_MS: u64 = 200;
/// Chart and series resolution, ms.
pub const BUCKET_MS: u64 = 500;

/// How long and how often.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverloadSize {
    /// Seconds per run.
    pub run_secs: u64,
    /// Seeds in the simulator.
    pub sim_seeds: u64,
    /// Seeds on the real server (each one takes `run_secs` per limiter).
    pub real_seeds: u64,
}

impl OverloadSize {
    /// The published sizes.
    pub fn full() -> Self {
        Self {
            run_secs: 12,
            sim_seeds: 5,
            real_seeds: 3,
        }
    }

    /// For checking the pipeline; the numbers are not meaningful.
    pub fn quick() -> Self {
        Self {
            run_secs: 4,
            sim_seeds: 1,
            real_seeds: 1,
        }
    }
}

/// A named scenario.
#[derive(Debug, Clone)]
pub struct Scenario {
    /// Short name.
    pub name: &'static str,
    /// One sentence for the report.
    pub description: &'static str,
    /// The backend and load.
    pub config: OverloadConfig,
}

/// The two scenarios for runs of `run_secs`.
pub fn scenarios(run_secs: u64) -> Vec<Scenario> {
    let run = Duration::from_secs(run_secs);
    let capacity = WORKERS as u64 * 1_000 / SERVICE_MS;
    let base = OverloadConfig {
        duration: run,
        deadline: Duration::from_millis(DEADLINE_MS),
        service_mean: Duration::from_millis(SERVICE_MS),
        load: vec![(Duration::ZERO, capacity / 4), (run, capacity * 3)],
        workers: vec![(Duration::ZERO, WORKERS)],
    };
    vec![
        Scenario {
            name: "ramp",
            description: "offered load rises steadily from a quarter of the backend's capacity to three times it",
            config: base.clone(),
        },
        Scenario {
            name: "capacity_change",
            description: "constant load of 1.5× the healthy capacity while the backend loses half its workers, then scales out to twice the healthy count",
            config: OverloadConfig {
                load: vec![(Duration::ZERO, capacity * 3 / 2)],
                workers: vec![
                    (Duration::ZERO, WORKERS),
                    (run / 3, WORKERS / 2),
                    (run * 2 / 3, WORKERS * 2),
                ],
                ..base
            },
        },
    ]
}

/// The limits compared: `None` is no limit.
pub fn limiters(deadline: Duration) -> Vec<(&'static str, Option<Concurrency>)> {
    vec![
        ("none", None),
        ("static", Some(Concurrency::Fixed(WORKERS))),
        (
            "aimd",
            Some(Concurrency::Aimd(AimdConfig {
                latency_threshold: deadline,
                ..AimdConfig::default()
            })),
        ),
        (
            "gradient2",
            Some(Concurrency::Gradient2(Gradient2Config::default())),
        ),
    ]
}

/// Runs `requests` through the real layer and backend, in real time.
pub async fn real_run(
    config: &OverloadConfig,
    requests: &[Request],
    limit: Option<Concurrency>,
) -> Vec<Served> {
    use tower::ServiceExt;
    let backend = CapacityBackend::new(config.workers_at(Nanos(0)));
    let mut app = work_router(Arc::clone(&backend), config.deadline);
    if let Some(c) = limit {
        app = app.layer(ConcurrencyLimitLayer::new(Arc::new(
            ConcurrencyLimiter::new(c),
        )));
    }
    let start = tokio::time::Instant::now();
    for &(at, n) in config.workers.iter().skip(1) {
        let b = Arc::clone(&backend);
        tokio::spawn(async move {
            tokio::time::sleep_until(start + at).await;
            b.set_workers(n);
        });
    }
    let mut handles = Vec::with_capacity(requests.len());
    for r in requests {
        tokio::time::sleep_until(start + Duration::from_nanos(r.at.0)).await;
        let app = app.clone();
        let (at, us) = (r.at, r.service.as_micros());
        handles.push(tokio::spawn(async move {
            let request = axum::http::Request::get("/work")
                .header(WORK_HEADER, us.to_string())
                .body(axum::body::Body::empty())
                .unwrap_or_default();
            let sent = tokio::time::Instant::now();
            let response = app.oneshot(request).await;
            let latency = sent.elapsed();
            let (status, limit) = match response {
                Ok(r) => (
                    r.status().as_u16(),
                    r.headers()
                        .get(LIMIT_HEADER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse().ok()),
                ),
                Err(never) => match never {},
            };
            Served {
                at,
                latency,
                status: match status {
                    200 => Status::Ok,
                    503 => Status::Shed,
                    _ => Status::Timeout,
                },
                limit,
            }
        }));
    }
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        if let Ok(s) = h.await {
            out.push(s);
        }
    }
    out
}

/// Means and spreads over seeds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agg {
    /// `200`s per second.
    pub goodput_rps: Summary,
    /// `200`s as a % of what could have succeeded.
    pub goodput_pct: Summary,
    /// Median latency of the `200`s, ms.
    pub p50_ms: Summary,
    /// p99 latency of the `200`s, ms.
    pub p99_ms: Summary,
    /// Shed, % of offered.
    pub shed_pct: Summary,
    /// Timed out, % of offered.
    pub timeout_pct: Summary,
}

impl Agg {
    /// Summarises runs.
    pub fn of(runs: &[OverloadMetrics]) -> Self {
        let s =
            |f: fn(&OverloadMetrics) -> f64| Summary::of(&runs.iter().map(f).collect::<Vec<_>>());
        let pct = |n: u64, m: &OverloadMetrics| {
            if m.offered == 0 {
                0.0
            } else {
                n as f64 * 100.0 / m.offered as f64
            }
        };
        Self {
            goodput_rps: s(|m| m.goodput_rps),
            goodput_pct: s(|m| m.goodput_pct),
            p50_ms: s(|m| m.p50_ms),
            p99_ms: s(|m| m.p99_ms),
            shed_pct: Summary::of(&runs.iter().map(|m| pct(m.shed, m)).collect::<Vec<_>>()),
            timeout_pct: Summary::of(&runs.iter().map(|m| pct(m.timeout, m)).collect::<Vec<_>>()),
        }
    }
}

/// One limiter in one scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Row {
    /// `none`, `static`, `aimd` or `gradient2`.
    pub limiter: String,
    /// Simulated, over `sim_seeds`.
    pub sim: Agg,
    /// Real, over `real_seeds` (absent with `--sim-only`).
    pub real: Option<Agg>,
}

/// One bucket of a time series.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Point {
    /// Bucket start, seconds.
    pub t: f64,
    /// Offered requests per second.
    pub offered_rps: f64,
    /// `200`s per second.
    pub ok_rps: f64,
    /// Most that could have succeeded, per second.
    pub possible_rps: f64,
    /// Mean concurrency limit.
    pub limit: Option<f64>,
    /// p99 latency of the `200`s, ms.
    pub p99_ms: f64,
}

/// Seed 1 of one limiter over time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Line {
    /// Limiter name.
    pub limiter: String,
    /// `real` or `simulated`.
    pub source: String,
    /// The buckets.
    pub points: Vec<Point>,
}

/// One scenario's results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    /// Short name.
    pub name: String,
    /// What happens.
    pub description: String,
    /// Offered load `(seconds, requests per second)`.
    pub load: Vec<(f64, u64)>,
    /// Workers `(from seconds, workers)`.
    pub workers: Vec<(f64, usize)>,
    /// One row per limiter.
    pub rows: Vec<Row>,
    /// Seed 1 over time, real and simulated.
    pub series: Vec<Line>,
}

/// `results/overload.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverloadResults {
    /// File format version.
    pub schema: u32,
    /// Where it ran.
    pub machine: Machine,
    /// Run lengths and seeds.
    pub size: OverloadSize,
    /// Healthy backend workers.
    pub workers: usize,
    /// Mean service time, ms.
    pub service_ms: u64,
    /// Deadline, ms.
    pub deadline_ms: u64,
    /// Series resolution, ms.
    pub bucket_ms: u64,
    /// The scenarios.
    pub scenarios: Vec<ScenarioResult>,
}

fn line(limiter: &str, source: &str, m: &OverloadMetrics) -> Line {
    let per_sec = 1_000.0 / BUCKET_MS as f64;
    Line {
        limiter: limiter.to_owned(),
        source: source.to_owned(),
        points: m
            .series
            .iter()
            .map(|b| Point {
                t: b.start_secs,
                offered_rps: b.offered as f64 * per_sec,
                ok_rps: b.ok as f64 * per_sec,
                possible_rps: b.possible * per_sec,
                limit: b.limit,
                p99_ms: b.p99_ms,
            })
            .collect(),
    }
}

/// Runs every scenario and limiter. `real` adds the real-time runs.
pub fn run(size: &OverloadSize, real: bool, step: impl Fn(&str)) -> Vec<ScenarioResult> {
    let bucket = Duration::from_millis(BUCKET_MS);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .ok();
    let real = real && runtime.is_some();
    scenarios(size.run_secs)
        .into_iter()
        .map(|sc| {
            let c = &sc.config;
            let mut rows = Vec::new();
            let mut series = Vec::new();
            for (name, limit) in limiters(c.deadline) {
                step(&format!("{}: {name}", sc.name));
                let mut sim_runs = Vec::new();
                for seed in 1..=size.sim_seeds {
                    let reqs = overload::requests(seed, c);
                    let limiter = limit.map(ConcurrencyLimiter::new);
                    let m =
                        overload::measure(c, &overload::run(c, &reqs, limiter.as_ref()), bucket);
                    if seed == 1 {
                        series.push(line(name, "simulated", &m));
                    }
                    sim_runs.push(m);
                }
                let real_runs: Option<Vec<OverloadMetrics>> = match (&runtime, real) {
                    (Some(rt), true) => Some(
                        (1..=size.real_seeds)
                            .map(|seed| {
                                let reqs = overload::requests(seed, c);
                                let served = rt.block_on(real_run(c, &reqs, limit));
                                let m = overload::measure(c, &served, bucket);
                                if seed == 1 {
                                    series.push(line(name, "real", &m));
                                }
                                m
                            })
                            .collect(),
                    ),
                    _ => None,
                };
                rows.push(Row {
                    limiter: name.to_owned(),
                    sim: Agg::of(&sim_runs),
                    real: real_runs.as_deref().map(Agg::of),
                });
            }
            ScenarioResult {
                name: sc.name.to_owned(),
                description: sc.description.to_owned(),
                load: c.load.iter().map(|&(t, r)| (t.as_secs_f64(), r)).collect(),
                workers: c
                    .workers
                    .iter()
                    .map(|&(t, w)| (t.as_secs_f64(), w))
                    .collect(),
                rows,
                series,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_match_the_constants() {
        let s = scenarios(12);
        assert_eq!(s.len(), 2);
        let capacity = s[0].config.capacity_at(Nanos(0));
        assert_eq!(capacity, 800.0);
        assert_eq!(
            s[0].config
                .rate_at(Nanos::from_duration(Duration::from_secs(12))),
            2_400
        );
        let at = |secs| Nanos::from_duration(Duration::from_secs(secs));
        assert_eq!(s[1].config.workers_at(at(5)), 8);
        assert_eq!(s[1].config.workers_at(at(9)), 32);
    }

    #[test]
    fn the_real_layer_matches_the_simulator_on_a_light_load() {
        // Light load, paused Tokio time: no queueing, so both sides answer every
        // request with 200 and the same latency (the service time).
        let config = OverloadConfig {
            duration: Duration::from_secs(1),
            deadline: Duration::from_millis(DEADLINE_MS),
            service_mean: Duration::from_millis(SERVICE_MS),
            load: vec![(Duration::ZERO, 20)],
            workers: vec![(Duration::ZERO, WORKERS)],
        };
        let reqs = overload::requests(1, &config);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let real = rt.block_on(real_run(&config, &reqs, Some(Concurrency::Fixed(WORKERS))));
        let limiter = ConcurrencyLimiter::new(Concurrency::Fixed(WORKERS));
        let simulated = overload::run(&config, &reqs, Some(&limiter));
        assert_eq!(real.len(), simulated.len());
        for (r, s) in real.iter().zip(&simulated) {
            assert_eq!(r.status, Status::Ok);
            assert_eq!(s.status, Status::Ok);
            assert_eq!(r.limit, Some(WORKERS));
            // Tokio timers round up to the millisecond.
            let gap = r.latency.abs_diff(s.latency);
            assert!(
                gap <= Duration::from_millis(2),
                "{:?} vs {:?}",
                r.latency,
                s.latency
            );
        }
    }

    #[test]
    fn quick_run_produces_every_row_and_series() {
        let size = OverloadSize {
            run_secs: 2,
            sim_seeds: 2,
            real_seeds: 1,
        };
        let out = run(&size, false, |_| {});
        assert_eq!(out.len(), 2);
        for sc in &out {
            assert_eq!(sc.rows.len(), 4);
            assert!(
                sc.rows
                    .iter()
                    .all(|r| r.real.is_none() && r.sim.goodput_pct.n == 2)
            );
            assert_eq!(sc.series.len(), 4);
            assert_eq!(sc.series[0].points.len(), 4);
        }
    }
}
