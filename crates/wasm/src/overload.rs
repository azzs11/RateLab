//! Overload: a backend with limited capacity behind no limit, a static concurrency
//! limit, AIMD or Gradient2, from [`sim::overload`]. The same model as the benchmark's
//! simulated runs, with the scenario's knobs exposed.

use ratelab::concurrency::{AimdConfig, Concurrency, ConcurrencyLimiter, Gradient2Config};
use sim::overload::{OverloadConfig, measure, requests, run};
use std::time::Duration;
use wasm_bindgen::prelude::*;

/// One run, as per-bucket series plus totals.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq)]
pub struct OverloadRun {
    t: Vec<f64>,
    offered: Vec<f64>,
    ok: Vec<f64>,
    possible: Vec<f64>,
    limit: Vec<f64>,
    workers: Vec<f64>,
    goodput_pct: f64,
    p50_ms: f64,
    p99_ms: f64,
    shed_pct: f64,
    timeout_pct: f64,
}

/// The settings of one run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Setup<'a> {
    /// `none`, `static`, `aimd` or `gradient2`.
    pub limiter: &'a str,
    /// `ramp`, `capacity_change` or `steady`.
    pub scenario: &'a str,
    /// Healthy backend workers.
    pub workers: u32,
    /// Mean service time, ms.
    pub service_ms: f64,
    /// Server deadline, ms.
    pub deadline_ms: f64,
    /// Offered load (peak, for the ramp) as a multiple of the healthy capacity.
    pub load: f64,
    /// Run length, seconds.
    pub secs: u32,
    /// The static limit.
    pub static_limit: u32,
    /// Seed.
    pub seed: u32,
}

/// The scenario as a simulator configuration.
pub fn config(s: &Setup) -> Result<OverloadConfig, String> {
    if s.workers == 0 || s.service_ms <= 0.0 || s.deadline_ms <= 0.0 || s.secs == 0 {
        return Err("workers, service time, deadline and length must be above zero".into());
    }
    if !(0.05..=20.0).contains(&s.load) {
        return Err("load must be between 0.05 and 20 times the capacity".into());
    }
    let run = Duration::from_secs(u64::from(s.secs));
    let workers = s.workers as usize;
    let capacity = f64::from(s.workers) * 1_000.0 / s.service_ms;
    let rate = |x: f64| (capacity * x).round().max(1.0) as u64;
    let (load, steps) = match s.scenario {
        "ramp" => (
            vec![(Duration::ZERO, rate(0.25)), (run, rate(s.load))],
            vec![(Duration::ZERO, workers)],
        ),
        "capacity_change" => (
            vec![(Duration::ZERO, rate(s.load))],
            vec![
                (Duration::ZERO, workers),
                (run / 3, (workers / 2).max(1)),
                (run * 2 / 3, workers * 2),
            ],
        ),
        "steady" => (
            vec![(Duration::ZERO, rate(s.load))],
            vec![(Duration::ZERO, workers)],
        ),
        other => return Err(format!("unknown scenario {other:?}")),
    };
    Ok(OverloadConfig {
        duration: run,
        deadline: Duration::from_secs_f64(s.deadline_ms / 1_000.0),
        service_mean: Duration::from_secs_f64(s.service_ms / 1_000.0),
        load,
        workers: steps,
    })
}

fn limiter(s: &Setup, deadline: Duration) -> Result<Option<Concurrency>, String> {
    Ok(match s.limiter {
        "none" => None,
        "static" => Some(Concurrency::Fixed(s.static_limit.max(1) as usize)),
        "aimd" => Some(Concurrency::Aimd(AimdConfig {
            latency_threshold: deadline,
            ..AimdConfig::default()
        })),
        "gradient2" => Some(Concurrency::Gradient2(Gradient2Config::default())),
        other => return Err(format!("unknown limiter {other:?}")),
    })
}

impl OverloadRun {
    /// Runs one setup, in buckets of `bucket_ms`.
    pub fn build(s: &Setup, bucket_ms: f64) -> Result<OverloadRun, String> {
        let c = config(s)?;
        let concurrency = limiter(s, c.deadline)?;
        let reqs = requests(u64::from(s.seed), &c);
        if reqs.len() > 2_000_000 {
            return Err("too many requests: shorten the run or lower the load".into());
        }
        let l = concurrency.map(ConcurrencyLimiter::new);
        let served = run(&c, &reqs, l.as_ref());
        let bucket = Duration::from_secs_f64(bucket_ms.max(50.0) / 1_000.0);
        let m = measure(&c, &served, bucket);
        let per_sec = 1.0 / bucket.as_secs_f64();
        let pct = |n: u64| {
            if m.offered == 0 {
                0.0
            } else {
                n as f64 * 100.0 / m.offered as f64
            }
        };
        Ok(OverloadRun {
            t: m.series.iter().map(|b| b.start_secs).collect(),
            offered: m
                .series
                .iter()
                .map(|b| b.offered as f64 * per_sec)
                .collect(),
            ok: m.series.iter().map(|b| b.ok as f64 * per_sec).collect(),
            possible: m.series.iter().map(|b| b.possible * per_sec).collect(),
            limit: m
                .series
                .iter()
                .map(|b| b.limit.unwrap_or(f64::NAN))
                .collect(),
            workers: m
                .series
                .iter()
                .map(|b| c.workers_at(ratelab::Nanos((b.start_secs * 1e9) as u64)) as f64)
                .collect(),
            goodput_pct: m.goodput_pct,
            p50_ms: m.p50_ms,
            p99_ms: m.p99_ms,
            shed_pct: pct(m.shed),
            timeout_pct: pct(m.timeout),
        })
    }
}

#[wasm_bindgen]
impl OverloadRun {
    /// Runs `limiter` (`none`, `static`, `aimd`, `gradient2`) on `scenario` (`ramp`,
    /// `capacity_change`, `steady`). Throws on bad settings.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        limiter: &str,
        scenario: &str,
        workers: u32,
        service_ms: f64,
        deadline_ms: f64,
        load: f64,
        secs: u32,
        static_limit: u32,
        seed: u32,
        bucket_ms: f64,
    ) -> Result<OverloadRun, JsError> {
        let s = Setup {
            limiter,
            scenario,
            workers,
            service_ms,
            deadline_ms,
            load,
            secs,
            static_limit,
            seed,
        };
        OverloadRun::build(&s, bucket_ms).map_err(|e| JsError::new(&e))
    }

    /// Bucket start times, seconds.
    #[wasm_bindgen(js_name = t)]
    pub fn times(&self) -> Vec<f64> {
        self.t.clone()
    }
    /// Offered requests per second, per bucket.
    pub fn offered(&self) -> Vec<f64> {
        self.offered.clone()
    }
    /// Successful answers per second, per bucket.
    pub fn ok(&self) -> Vec<f64> {
        self.ok.clone()
    }
    /// The most that could have succeeded per second, per bucket.
    pub fn possible(&self) -> Vec<f64> {
        self.possible.clone()
    }
    /// Mean concurrency limit per bucket (NaN without a limiter).
    pub fn limit(&self) -> Vec<f64> {
        self.limit.clone()
    }
    /// Backend workers per bucket.
    pub fn workers(&self) -> Vec<f64> {
        self.workers.clone()
    }
    /// Successful answers as a % of what could have succeeded.
    #[wasm_bindgen(js_name = goodputPct)]
    pub fn goodput_pct(&self) -> f64 {
        self.goodput_pct
    }
    /// Median latency of the successful answers, ms.
    #[wasm_bindgen(js_name = p50Ms)]
    pub fn p50_ms(&self) -> f64 {
        self.p50_ms
    }
    /// p99 latency of the successful answers, ms.
    #[wasm_bindgen(js_name = p99Ms)]
    pub fn p99_ms(&self) -> f64 {
        self.p99_ms
    }
    /// Shed (503), % of offered.
    #[wasm_bindgen(js_name = shedPct)]
    pub fn shed_pct(&self) -> f64 {
        self.shed_pct
    }
    /// Timed out (504), % of offered.
    #[wasm_bindgen(js_name = timeoutPct)]
    pub fn timeout_pct(&self) -> f64 {
        self.timeout_pct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(limiter: &'static str, scenario: &'static str) -> Setup<'static> {
        Setup {
            limiter,
            scenario,
            workers: 16,
            service_ms: 20.0,
            deadline_ms: 200.0,
            load: 3.0,
            secs: 12,
            static_limit: 16,
            seed: 1,
        }
    }

    #[test]
    fn matches_the_benchmark_scenarios() {
        let c = config(&setup("none", "ramp")).unwrap();
        assert_eq!(c.load.last().unwrap().1, 2_400);
        let c = config(&Setup {
            load: 1.5,
            ..setup("none", "capacity_change")
        })
        .unwrap();
        assert_eq!(
            c.workers.iter().map(|w| w.1).collect::<Vec<_>>(),
            [16, 8, 32]
        );
    }

    #[test]
    fn gradient2_beats_no_limit_on_the_ramp() {
        let none = OverloadRun::build(&setup("none", "ramp"), 500.0).unwrap();
        let g2 = OverloadRun::build(&setup("gradient2", "ramp"), 500.0).unwrap();
        assert!(g2.goodput_pct > none.goodput_pct * 1.5);
        assert_eq!(none.t.len(), 24);
        assert!(none.limit.iter().all(|l| l.is_nan()));
        assert!(g2.limit.iter().any(|l| l.is_finite()));
    }

    #[test]
    fn bad_settings_are_errors() {
        assert!(
            config(&Setup {
                workers: 0,
                ..setup("none", "ramp")
            })
            .is_err()
        );
        assert!(
            OverloadRun::build(
                &Setup {
                    limiter: "magic",
                    ..setup("none", "ramp")
                },
                500.0
            )
            .is_err()
        );
    }
}
