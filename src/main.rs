use distributed_rate_limiter::algorithms::*;
use distributed_rate_limiter::metrics::{self, record_request};
use distributed_rate_limiter::redis_limiter::RedisRateLimiter;
use distributed_rate_limiter::{RateLimitConfig, RateLimiter};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    // Initialize metrics
    metrics::init_metrics();

    println!("🚀 Distributed Rate Limiter - Demo");
    println!("======================================\n");

    let config = RateLimitConfig::per_second(3);

    println!("Testing all 4 rate limiting algorithms WITH METRICS");
    println!("Config: 3 requests per second\n");

    // Test 1: Token Bucket
    println!("1️⃣  TOKEN BUCKET");
    println!("   (Tokens refill over time)");
    println!("   ----------------------------");
    test_algorithm_with_metrics("Token Bucket", TokenBucket::new(config.clone())).await;

    println!("\n");

    // Test 2: Leaky Bucket
    println!("2️⃣  LEAKY BUCKET");
    println!("   (Queue that leaks at constant rate)");
    println!("   ----------------------------");
    test_algorithm_with_metrics("Leaky Bucket", LeakyBucket::new(config.clone())).await;

    println!("\n");

    // Test 3: Fixed Window
    println!("3️⃣  FIXED WINDOW");
    println!("   (Counter resets every window)");
    println!("   ----------------------------");
    test_algorithm_with_metrics("Fixed Window", FixedWindow::new(config.clone())).await;

    println!("\n");

    // Test 4: Sliding Window
    println!("4️⃣  SLIDING WINDOW");
    println!("   (Tracks exact timestamps)");
    println!("   ----------------------------");
    test_algorithm_with_metrics("Sliding Window", SlidingWindow::new(config.clone())).await;

    println!("\n");

    // Test 5: Redis Distributed
    println!("5️⃣  REDIS DISTRIBUTED");
    println!("   (Token Bucket with Redis backend)");
    println!("   ----------------------------");
    test_redis().await;

    println!("\n");
    println!("✨ Demo complete.");

    // Print metrics summary
    metrics::print_metrics_summary();

    println!("\n💡 Measure throughput on your machine: cargo bench");

    // Show how to access metrics
    println!("\n🔍 Prometheus Metrics Format:");
    println!("==============================");
    let metrics_output = metrics::get_metrics();
    println!("{}", metrics_output);
}

async fn test_algorithm_with_metrics<T: RateLimiter>(_name: &str, mut limiter: T) {
    for i in 1..=5 {
        let start = Instant::now();
        let allowed = limiter.allow_request("user_test").unwrap();
        record_request(allowed, start);

        println!(
            "   Request {}: {}",
            i,
            if allowed {
                "✅ ALLOWED"
            } else {
                "❌ BLOCKED"
            }
        );
    }

    println!("   ⏳ Waiting 1 second...");
    tokio::time::sleep(Duration::from_secs(1)).await;

    for i in 1..=2 {
        let start = Instant::now();
        let allowed = limiter.allow_request("user_test").unwrap();
        record_request(allowed, start);

        println!(
            "   Request {}: {}",
            i,
            if allowed {
                "✅ ALLOWED"
            } else {
                "❌ BLOCKED"
            }
        );
    }
}

async fn test_redis() {
    let redis_url = "redis://127.0.0.1/";
    let config = RateLimitConfig::per_second(3);

    match RedisRateLimiter::new(redis_url, config) {
        Ok(mut limiter) => {
            println!("   ✅ Connected to Redis\n");
            limiter.reset("user_redis");

            for i in 1..=5 {
                match limiter.allow_request("user_redis") {
                    Ok(allowed) => {
                        let start = Instant::now();
                        record_request(allowed, start);
                        println!(
                            "   Request {}: {}",
                            i,
                            if allowed {
                                "✅ ALLOWED"
                            } else {
                                "❌ BLOCKED"
                            }
                        );
                    }
                    Err(_) => {
                        println!("   ⚠️  Redis connection lost during test");
                        break;
                    }
                }
            }

            limiter.reset("user_redis");
        }
        Err(_) => {
            println!("   ⚠️  Redis not available (start Redis server to test distributed mode)");
            println!("   💡 Run: redis-server.exe");
        }
    }
}
