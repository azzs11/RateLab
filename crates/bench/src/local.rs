//! In-process measurements: accuracy against the oracle, decision latency, throughput
//! across threads and key counts, and fairness between tenants.
//!
//! Accuracy and fairness replay seeded scenarios on a [`ManualClock`], so they are
//! exactly reproducible. Latency and throughput use the real clock and real threads;
//! each configuration is warmed up once and then repeated, and the spread is reported.

use crate::oracle::{ExactOracle, compare};
use crate::stats::{Summary, jain, percentile};
use drl_core::{Algorithm, Cost, ManualClock, MonotonicClock, Nanos, RateLimitConfig, RateLimiter};
use serde::{Deserialize, Serialize};
use sim::{Arrival, SplitMix64};
use std::hint::black_box;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

/// How much work each measurement does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteSize {
    /// Name of this size (`full` or `quick`).
    pub name: String,
    /// Scenario length for accuracy and fairness.
    pub scenario_secs: u64,
    /// Seeds per scenario.
    pub seeds: u64,
    /// Timed batches per latency run.
    pub latency_samples: usize,
    /// Repetitions after warm-up, for latency and throughput.
    pub runs: usize,
    /// Checks each thread makes per throughput run.
    pub checks_per_thread: usize,
    /// Thread counts for throughput.
    pub threads: Vec<usize>,
    /// Distinct key counts for throughput.
    pub keys: Vec<usize>,
}

impl SuiteSize {
    /// The size used for RESULTS.md.
    pub fn full() -> Self {
        Self {
            name: "full".to_owned(),
            scenario_secs: 30,
            seeds: 5,
            latency_samples: 20_000,
            runs: 5,
            checks_per_thread: 200_000,
            threads: vec![1, 4, 8, 16],
            keys: vec![1, 1_000, 1_000_000],
        }
    }

    /// A fast size for CI: checks the pipeline end to end, numbers are not meaningful.
    pub fn quick() -> Self {
        Self {
            name: "quick".to_owned(),
            scenario_secs: 2,
            seeds: 2,
            latency_samples: 200,
            runs: 2,
            checks_per_thread: 2_000,
            threads: vec![1, 4],
            keys: vec![1, 1_000],
        }
    }
}

/// Limit used by the accuracy scenarios: 100 units per second per key.
pub const ACCURACY_LIMIT: u64 = 100;
/// Clients in the accuracy scenarios.
pub const ACCURACY_KEYS: u32 = 10;

/// A traffic pattern from the plan's scenario list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Poisson arrivals at 2× the limit per key.
    SteadyPoisson,
    /// 1 s on at 4× the limit, 1 s off.
    OnOff,
    /// Pareto (shape 2) gaps, mean 2× the limit.
    HeavyTailed,
    /// Rate ramps from 0.5× to 4× the limit and back over the scenario.
    Diurnal,
    /// 1× the limit, then 10× as many clients and requests for the middle third.
    FlashCrowd,
    /// `limit` requests just before a window boundary and `limit` just after.
    BoundaryAttack,
}

impl Scenario {
    /// Every scenario, in report order.
    pub const ALL: [Scenario; 6] = [
        Scenario::SteadyPoisson,
        Scenario::OnOff,
        Scenario::HeavyTailed,
        Scenario::Diurnal,
        Scenario::FlashCrowd,
        Scenario::BoundaryAttack,
    ];

    /// Snake-case name.
    pub fn name(self) -> &'static str {
        match self {
            Scenario::SteadyPoisson => "steady_poisson",
            Scenario::OnOff => "on_off",
            Scenario::HeavyTailed => "heavy_tailed",
            Scenario::Diurnal => "diurnal",
            Scenario::FlashCrowd => "flash_crowd",
            Scenario::BoundaryAttack => "boundary_attack",
        }
    }

    /// Whether the scenario uses randomness (the boundary attack does not).
    pub fn is_seeded(self) -> bool {
        self != Scenario::BoundaryAttack
    }

    /// The arrivals for `seed`, lasting `secs` seconds.
    pub fn arrivals(self, seed: u64, secs: u64) -> Vec<Arrival> {
        let d = Duration::from_secs(secs);
        let keys = ACCURACY_KEYS;
        // Mean gap between requests of one key at `x` times the limit, across all keys.
        let gap_at = |x_tenths: u64| {
            Duration::from_nanos(10 * 1_000_000_000 / (ACCURACY_LIMIT * x_tenths * u64::from(keys)))
        };
        match self {
            Scenario::SteadyPoisson => sim::poisson(seed, d, gap_at(20), keys),
            Scenario::OnOff => sim::on_off(
                seed,
                d,
                gap_at(40),
                Duration::from_secs(1),
                Duration::from_secs(1),
                keys,
            ),
            Scenario::HeavyTailed => sim::pareto(seed, d, gap_at(20), keys),
            Scenario::Diurnal => sim::diurnal(seed, d, gap_at(5), gap_at(40), d, keys),
            Scenario::FlashCrowd => sim::flash_crowd(seed, d, gap_at(10), keys, d / 3, d / 3, 10),
            Scenario::BoundaryAttack => {
                let config = accuracy_config();
                sim::boundary_attack(config, Duration::from_millis(2))
            }
        }
    }
}

fn accuracy_config() -> RateLimitConfig {
    // Constant, non-zero inputs: cannot fail.
    RateLimitConfig::per_second(ACCURACY_LIMIT).expect("constant, valid config")
}

/// Accuracy of one algorithm on one scenario, over every seed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccuracyRow {
    /// Algorithm name.
    pub algorithm: String,
    /// Scenario name.
    pub scenario: String,
    /// Requests per run.
    pub requests: Summary,
    /// % of requests allowed that the oracle denied.
    pub over_pct: Summary,
    /// % of requests denied that the oracle allowed.
    pub under_pct: Summary,
    /// Admitted vs oracle admitted, in %.
    pub admitted_vs_oracle_pct: Summary,
    /// Worst units in one window / limit.
    pub worst_window_ratio: Summary,
}

/// Replays every scenario through every algorithm and compares with the oracle.
pub fn accuracy(size: &SuiteSize) -> Vec<AccuracyRow> {
    let config = accuracy_config();
    let mut rows = Vec::new();
    for scenario in Scenario::ALL {
        let seeds = if scenario.is_seeded() { size.seeds } else { 1 };
        let inputs: Vec<(Vec<Arrival>, Vec<bool>)> = (1..=seeds)
            .map(|seed| {
                let arrivals = scenario.arrivals(seed, size.scenario_secs);
                let oracle =
                    ExactOracle::decide_all(config.limit(), config.window_nanos(), &arrivals);
                (arrivals, oracle)
            })
            .collect();
        for algorithm in Algorithm::ALL {
            let results: Vec<_> = inputs
                .iter()
                .map(|(arrivals, oracle)| {
                    let clock = Arc::new(ManualClock::new());
                    let limiter = algorithm.build(config, Arc::clone(&clock));
                    let trace = sim::run(&limiter, &clock, arrivals);
                    compare(&trace, oracle, config.limit(), config.window_nanos())
                })
                .collect();
            let col = |f: fn(&crate::oracle::Accuracy) -> f64| {
                Summary::of(&results.iter().map(f).collect::<Vec<_>>())
            };
            rows.push(AccuracyRow {
                algorithm: algorithm.name().to_owned(),
                scenario: scenario.name().to_owned(),
                requests: col(|a| a.requests as f64),
                over_pct: col(|a| a.over_pct),
                under_pct: col(|a| a.under_pct),
                admitted_vs_oracle_pct: col(|a| a.admitted_vs_oracle_pct),
                worst_window_ratio: col(|a| a.worst_window_ratio),
            });
        }
    }
    rows
}

/// Calls timed together in one latency sample.
pub const LATENCY_BATCH: usize = 100;

/// The clock's resolution and the cost of reading it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TimerInfo {
    /// Smallest non-zero difference between two clock reads, in nanoseconds.
    pub resolution_ns: u64,
    /// Median time of an empty timed section, in nanoseconds.
    pub overhead_ns: u64,
}

/// Measures the timer every latency sample relies on.
pub fn timer_info() -> TimerInfo {
    let mut samples: Vec<u64> = (0..100_000)
        .map(|_| {
            let t = Instant::now();
            black_box(t);
            t.elapsed().as_nanos() as u64
        })
        .collect();
    samples.sort_unstable();
    let resolution_ns = (0..10_000)
        .filter_map(|_| {
            let a = Instant::now();
            let mut b = Instant::now();
            while b == a {
                b = Instant::now();
            }
            u64::try_from((b - a).as_nanos()).ok()
        })
        .min()
        .unwrap_or(0);
    TimerInfo {
        resolution_ns,
        overhead_ns: percentile(&samples, 50.0),
    }
}

/// Time per `check` call of one algorithm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyRow {
    /// Algorithm name.
    pub algorithm: String,
    /// Distinct keys the calls were spread over.
    pub keys: usize,
    /// Calls per timed sample; each sample is the batch time divided by this.
    pub batch: usize,
    /// Median, in nanoseconds per call.
    pub p50_ns: Summary,
    /// 99th percentile, in nanoseconds per call.
    pub p99_ns: Summary,
    /// 99.9th percentile, in nanoseconds per call.
    pub p999_ns: Summary,
}

/// Times `check` on one thread over 1,000 keys, in batches of [`LATENCY_BATCH`] calls
/// (a single call is shorter than the clock's resolution on some machines).
pub fn latency(size: &SuiteSize) -> Vec<LatencyRow> {
    const KEYS: usize = 1_000;
    let keys: Vec<String> = (0..KEYS).map(|i| format!("user{i}")).collect();
    let config = RateLimitConfig::per_second(1_000).expect("constant, valid config");
    Algorithm::ALL
        .iter()
        .map(|&algorithm| {
            let limiter = algorithm.build(config, MonotonicClock::new());
            let one_run = || {
                let mut next = 0usize;
                // Per-call time in picoseconds, so integer percentiles keep precision.
                let mut samples: Vec<u64> = (0..size.latency_samples)
                    .map(|_| {
                        let t = Instant::now();
                        for _ in 0..LATENCY_BATCH {
                            black_box(limiter.check(black_box(&keys[next]), Cost::ONE));
                            next = (next + 1) % KEYS;
                        }
                        (t.elapsed().as_nanos() * 1_000 / LATENCY_BATCH as u128) as u64
                    })
                    .collect();
                samples.sort_unstable();
                let ns = |p| percentile(&samples, p) as f64 / 1_000.0;
                (ns(50.0), ns(99.0), ns(99.9))
            };
            one_run(); // warm-up
            let runs: Vec<_> = (0..size.runs).map(|_| one_run()).collect();
            let col = |f: fn(&(f64, f64, f64)) -> f64| {
                Summary::of(&runs.iter().map(f).collect::<Vec<_>>())
            };
            LatencyRow {
                algorithm: algorithm.name().to_owned(),
                keys: KEYS,
                batch: LATENCY_BATCH,
                p50_ns: col(|r| r.0),
                p99_ns: col(|r| r.1),
                p999_ns: col(|r| r.2),
            }
        })
        .collect()
}

/// Decisions per second for one algorithm, thread count and key count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThroughputRow {
    /// Algorithm name.
    pub algorithm: String,
    /// Threads calling `check` at once on one shared limiter.
    pub threads: usize,
    /// Distinct keys, chosen uniformly at random per call.
    pub keys: usize,
    /// Total decisions per second across all threads.
    pub decisions_per_sec: Summary,
}

fn throughput_run(
    limiter: &dyn RateLimiter,
    keys: &[String],
    threads: usize,
    checks: usize,
    seed: u64,
) -> f64 {
    let barrier = Barrier::new(threads + 1);
    let elapsed = std::thread::scope(|s| {
        for t in 0..threads {
            let barrier = &barrier;
            s.spawn(move || {
                let mut rng = SplitMix64::new(seed.wrapping_mul(31).wrapping_add(t as u64));
                let n = keys.len() as u64;
                barrier.wait();
                for _ in 0..checks {
                    let key = &keys[rng.below(n) as usize];
                    black_box(limiter.check(key, Cost::ONE));
                }
            });
        }
        barrier.wait();
        Instant::now()
    })
    .elapsed();
    (threads * checks) as f64 / elapsed.as_secs_f64()
}

/// Shared-limiter throughput for every algorithm × thread count × key count.
pub fn throughput(size: &SuiteSize, mut progress: impl FnMut(&str)) -> Vec<ThroughputRow> {
    let config = RateLimitConfig::per_second(1_000).expect("constant, valid config");
    let mut rows = Vec::new();
    for &key_count in &size.keys {
        let keys: Vec<String> = (0..key_count).map(|i| format!("user{i}")).collect();
        for algorithm in Algorithm::ALL {
            progress(&format!("throughput {} keys={key_count}", algorithm.name()));
            let limiter = algorithm.build(config, MonotonicClock::new());
            // Touch every key once so runs measure steady state, not map growth.
            for key in &keys {
                limiter.check(key, Cost::ONE);
            }
            for &threads in &size.threads {
                throughput_run(&*limiter, &keys, threads, size.checks_per_thread, 0);
                let runs: Vec<f64> = (1..=size.runs as u64)
                    .map(|seed| {
                        throughput_run(&*limiter, &keys, threads, size.checks_per_thread, seed)
                    })
                    .collect();
                rows.push(ThroughputRow {
                    algorithm: algorithm.name().to_owned(),
                    threads,
                    keys: key_count,
                    decisions_per_sec: Summary::of(&runs),
                });
            }
        }
    }
    rows
}

/// Tenants in the fairness scenario.
pub const TENANTS: u32 = 10;

/// Fairness between tenants when one of them is noisy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FairnessRow {
    /// Algorithm name.
    pub algorithm: String,
    /// `global` (one shared key, 100/s) or `per_tenant` (one key each, 10/s).
    pub keying: String,
    /// Jain's index over admitted requests per tenant (1 = equal).
    pub jain: Summary,
    /// Share of all admitted requests that went to the noisy tenant, in %.
    pub noisy_share_pct: Summary,
}

/// Ten tenants share 100 requests/s. Tenant 0 sends 200/s, the others 20/s each, so
/// every tenant wants more than an equal share (10/s).
pub fn fairness(size: &SuiteSize) -> Vec<FairnessRow> {
    let secs = Duration::from_secs(size.scenario_secs);
    let mut rows = Vec::new();
    let traffic: Vec<Vec<Arrival>> = (1..=size.seeds)
        .map(|seed| {
            sim::merge((0..TENANTS).map(|tenant| {
                let gap = if tenant == 0 {
                    Duration::from_millis(5)
                } else {
                    Duration::from_millis(50)
                };
                sim::poisson(seed * 1_000 + u64::from(tenant), secs, gap, 1)
                    .into_iter()
                    .map(|a| Arrival { key: tenant, ..a })
                    .collect()
            }))
        })
        .collect();
    for (keying, limit) in [("global", 100), ("per_tenant", 10)] {
        let config = RateLimitConfig::per_second(limit).expect("constant, valid config");
        for algorithm in Algorithm::ALL {
            let mut jains = Vec::new();
            let mut shares = Vec::new();
            for arrivals in &traffic {
                let clock = Arc::new(ManualClock::new());
                let limiter = algorithm.build(config, Arc::clone(&clock));
                let mut admitted = vec![0f64; TENANTS as usize];
                for a in arrivals {
                    clock.set(Nanos(a.at.0));
                    let key = if keying == "global" {
                        "global".to_owned()
                    } else {
                        format!("t{}", a.key)
                    };
                    if limiter.check(&key, a.cost).allowed {
                        admitted[a.key as usize] += 1.0;
                    }
                }
                let total: f64 = admitted.iter().sum();
                jains.push(jain(&admitted));
                shares.push(if total > 0.0 {
                    admitted[0] * 100.0 / total
                } else {
                    0.0
                });
            }
            rows.push(FairnessRow {
                algorithm: algorithm.name().to_owned(),
                keying: keying.to_owned(),
                jain: Summary::of(&jains),
                noisy_share_pct: Summary::of(&shares),
            });
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_offer_more_than_the_limit() {
        // 2× the limit for 10 keys at 100/s is about 2,000 requests per second.
        let a = Scenario::SteadyPoisson.arrivals(1, 5);
        assert!((9_000..11_000).contains(&a.len()), "{}", a.len());
        for s in Scenario::ALL {
            assert!(!s.arrivals(1, 3).is_empty(), "{}", s.name());
        }
    }

    #[test]
    fn quick_suite_produces_every_row() {
        let size = SuiteSize::quick();
        let acc = accuracy(&size);
        assert_eq!(acc.len(), Scenario::ALL.len() * Algorithm::ALL.len());
        let sliding: Vec<_> = acc
            .iter()
            .filter(|r| r.algorithm == "sliding_window")
            .collect();
        assert!(
            sliding
                .iter()
                .all(|r| r.over_pct.max == 0.0 && r.under_pct.max == 0.0)
        );
        let fixed_attack = acc
            .iter()
            .find(|r| r.algorithm == "fixed_window" && r.scenario == "boundary_attack")
            .unwrap();
        assert_eq!(fixed_attack.worst_window_ratio.mean, 2.0);

        let fair = fairness(&size);
        let per_tenant = fair.iter().find(|r| r.keying == "per_tenant").unwrap();
        assert!(per_tenant.jain.mean > 0.95, "{:?}", per_tenant.jain);
        let global = fair.iter().find(|r| r.keying == "global").unwrap();
        assert!(
            global.noisy_share_pct.mean > 40.0,
            "{:?}",
            global.noisy_share_pct
        );
    }
}
