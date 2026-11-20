use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ratelab::{Algorithm, Cost, MonotonicClock, RateLimitConfig};
use std::hint::black_box;

/// Single-threaded decision time for one hot key, per algorithm and limit.
fn single_key(c: &mut Criterion) {
    for algorithm in Algorithm::ALL {
        let mut group = c.benchmark_group(algorithm.name());
        for limit in [100u64, 1_000, 10_000, 100_000] {
            let config = RateLimitConfig::per_second(limit).expect("valid config");
            let limiter = algorithm.build(config, MonotonicClock::new());
            group.bench_with_input(BenchmarkId::from_parameter(limit), &limit, |b, _| {
                b.iter(|| limiter.check(black_box("user1"), Cost::ONE));
            });
        }
        group.finish();
    }
}

/// All algorithms side by side at one limit.
fn comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("algorithm_comparison");
    let config = RateLimitConfig::per_second(1000).expect("valid config");
    for algorithm in Algorithm::ALL {
        let limiter = algorithm.build(config, MonotonicClock::new());
        group.bench_function(algorithm.name(), |b| {
            b.iter(|| limiter.check(black_box("user1"), Cost::ONE));
        });
    }
    group.finish();
}

criterion_group!(benches, single_key, comparison);
criterion_main!(benches);
