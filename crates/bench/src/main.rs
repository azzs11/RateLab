//! Measurement scenarios: `cargo run -p bench --release -- <scenario>`.
//!
//! Every number printed here is measured on the machine running it. Nothing is
//! asserted: speed depends on the hardware, so it does not belong in `cargo test`.
//! Phase 2 of the roadmap turns this into the full benchmark (exact oracle, JSON
//! output, generated RESULTS.md with machine specs).

use drl_core::{Algorithm, Cost, MonotonicClock, RateLimitConfig, RateLimiter};
use std::hint::black_box;
use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "usage: cargo run -p bench --release -- <scenario>

scenarios:
  load      single-threaded decisions per second, every algorithm, 100 keys
  latency   per-call time percentiles (includes timer overhead), every algorithm";

fn main() -> ExitCode {
    let scenario = std::env::args().nth(1);
    match scenario.as_deref() {
        Some("load") => load(),
        Some("latency") => latency(),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn limiter(algorithm: Algorithm, limit: u64) -> Box<dyn RateLimiter> {
    let config = RateLimitConfig::per_second(limit).expect("limit is non-zero");
    algorithm.build(config, MonotonicClock::new())
}

fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug build: numbers are not representative, use --release"
    } else {
        "release build"
    }
}

/// The old load test: 50,000 checks spread over 100 keys, one thread.
fn load() {
    const CHECKS: u64 = 50_000;
    const KEYS: u64 = 100;
    let keys: Vec<String> = (0..KEYS).map(|i| format!("user{i}")).collect();

    println!("scenario: load ({CHECKS} checks, {KEYS} keys, 1 thread, limit 10000/s)");
    println!("{}", build_profile());
    println!(
        "{:<15} {:>14} {:>9} {:>9}",
        "algorithm", "decisions/s", "allowed", "blocked"
    );
    for algorithm in Algorithm::ALL {
        let l = limiter(algorithm, 10_000);
        let mut allowed = 0u64;
        let start = Instant::now();
        for i in 0..CHECKS {
            if l.check(black_box(&keys[(i % KEYS) as usize]), Cost::ONE)
                .allowed
            {
                allowed += 1;
            }
        }
        let secs = start.elapsed().as_secs_f64();
        println!(
            "{:<15} {:>14.0} {:>9} {:>9}",
            algorithm.name(),
            CHECKS as f64 / secs,
            allowed,
            CHECKS - allowed
        );
    }
}

/// Per-call latency percentiles over 10,000 checks on 10 keys.
fn latency() {
    const CHECKS: usize = 10_000;
    let keys: Vec<String> = (0..10).map(|i| format!("user{i}")).collect();

    println!("scenario: latency ({CHECKS} checks, 10 keys, 1 thread, limit 1000/s)");
    println!("{}", build_profile());
    println!("each sample times one call with Instant, so it includes timer overhead");
    println!(
        "{:<15} {:>9} {:>9} {:>9}",
        "algorithm", "p50 ns", "p95 ns", "p99 ns"
    );
    for algorithm in Algorithm::ALL {
        let l = limiter(algorithm, 1_000);
        let mut samples: Vec<u128> = (0..CHECKS)
            .map(|i| {
                let start = Instant::now();
                black_box(l.check(&keys[i % keys.len()], Cost::ONE));
                start.elapsed().as_nanos()
            })
            .collect();
        samples.sort_unstable();
        let pct = |p: usize| samples[(samples.len() * p / 100).min(samples.len() - 1)];
        println!(
            "{:<15} {:>9} {:>9} {:>9}",
            algorithm.name(),
            pct(50),
            pct(95),
            pct(99)
        );
    }
}
