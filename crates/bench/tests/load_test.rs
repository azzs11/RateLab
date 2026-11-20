use drl_core::{Cost, FixedWindow, LeakyBucket, RateLimitConfig, RateLimiter, TokenBucket};
use std::time::Instant;

#[test]
fn load_test_token_bucket() {
    let config = RateLimitConfig::per_second(10000).unwrap();
    let limiter = TokenBucket::new(config);

    let start = Instant::now();
    let mut allowed = 0;
    let mut blocked = 0;

    // Simulate 50,000 requests
    for i in 0..50000 {
        let key = format!("user{}", i % 100); // 100 different users
        if limiter.check(&key, Cost::ONE).allowed {
            allowed += 1;
        } else {
            blocked += 1;
        }
    }

    let duration = start.elapsed();
    let requests_per_sec = 50000.0 / duration.as_secs_f64();

    println!("\n🔥 Token Bucket Load Test Results:");
    println!("   Total Requests: 50,000");
    println!("   Duration: {:.2}s", duration.as_secs_f64());
    println!("   Throughput: {:.0} req/s", requests_per_sec);
    println!("   Allowed: {}", allowed);
    println!("   Blocked: {}", blocked);

    // Should handle at least 10,000 req/s
    assert!(
        requests_per_sec > 10000.0,
        "Throughput too low: {} req/s",
        requests_per_sec
    );
}

#[test]
fn load_test_leaky_bucket() {
    let config = RateLimitConfig::per_second(10000).unwrap();
    let limiter = LeakyBucket::new(config);

    let start = Instant::now();
    let mut allowed = 0;
    let mut blocked = 0;

    for i in 0..50000 {
        let key = format!("user{}", i % 100);
        if limiter.check(&key, Cost::ONE).allowed {
            allowed += 1;
        } else {
            blocked += 1;
        }
    }

    let duration = start.elapsed();
    let requests_per_sec = 50000.0 / duration.as_secs_f64();

    println!("\n🔥 Leaky Bucket Load Test Results:");
    println!("   Total Requests: 50,000");
    println!("   Duration: {:.2}s", duration.as_secs_f64());
    println!("   Throughput: {:.0} req/s", requests_per_sec);
    println!("   Allowed: {}", allowed);
    println!("   Blocked: {}", blocked);

    assert!(requests_per_sec > 10000.0);
}

#[test]
fn load_test_fixed_window() {
    let config = RateLimitConfig::per_second(10000).unwrap();
    let limiter = FixedWindow::new(config);

    let start = Instant::now();
    let mut allowed = 0;
    let mut blocked = 0;

    for i in 0..50000 {
        let key = format!("user{}", i % 100);
        if limiter.check(&key, Cost::ONE).allowed {
            allowed += 1;
        } else {
            blocked += 1;
        }
    }

    let duration = start.elapsed();
    let requests_per_sec = 50000.0 / duration.as_secs_f64();

    println!("\n🔥 Fixed Window Load Test Results:");
    println!("   Total Requests: 50,000");
    println!("   Duration: {:.2}s", duration.as_secs_f64());
    println!("   Throughput: {:.0} req/s", requests_per_sec);
    println!("   Allowed: {}", allowed);
    println!("   Blocked: {}", blocked);

    assert!(requests_per_sec > 10000.0);
}

#[test]
fn latency_test() {
    let config = RateLimitConfig::per_second(1000).unwrap();
    let limiter = TokenBucket::new(config);

    let mut latencies = Vec::new();

    // Measure 1000 individual requests
    for i in 0..1000 {
        let start = Instant::now();
        let _ = limiter.check(&format!("user{}", i % 10), Cost::ONE);
        let latency = start.elapsed();
        latencies.push(latency.as_micros());
    }

    latencies.sort();

    let p50 = latencies[latencies.len() / 2];
    let p95 = latencies[latencies.len() * 95 / 100];
    let p99 = latencies[latencies.len() * 99 / 100];

    println!("\n⚡ Latency Test Results:");
    println!("   P50: {}µs", p50);
    println!("   P95: {}µs", p95);
    println!("   P99: {}µs", p99);

    // Should be sub-millisecond
    assert!(p99 < 1000, "P99 latency too high: {}µs", p99);
}
