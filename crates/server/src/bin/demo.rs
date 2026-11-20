use drl_core::{Algorithm, Cost, MonotonicClock, RateLimitConfig, RateLimiter};
use drl_redis_store::RedisRateLimiter;
use server::metrics::{self, record_request};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    metrics::init_metrics();

    println!("🚀 Distributed Rate Limiter - Demo");
    println!("======================================\n");

    let config = match RateLimitConfig::per_second(3) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("invalid config: {e}");
            return;
        }
    };

    println!(
        "Testing all {} rate limiting algorithms WITH METRICS",
        Algorithm::ALL.len()
    );
    println!("Config: 3 requests per second\n");

    let descriptions = [
        "(Tokens refill over time)",
        "(Queue that leaks at constant rate)",
        "(Counter resets every window)",
        "(Tracks exact timestamps)",
        "(One theoretical arrival time per key)",
    ];
    for (n, (algorithm, about)) in Algorithm::ALL.iter().zip(descriptions).enumerate() {
        println!(
            "{}. {}",
            n + 1,
            algorithm.name().replace('_', " ").to_uppercase()
        );
        println!("   {about}");
        println!("   ----------------------------");
        let limiter = algorithm.build(config, MonotonicClock::new());
        run_algorithm(&limiter).await;
        println!("\n");
    }

    println!("{}. REDIS (shared through Redis)", Algorithm::ALL.len() + 1);
    println!("   (Token bucket in Lua, on Redis TIME)");
    println!("   ----------------------------");
    run_redis(config).await;

    println!("\n");
    println!("✨ Demo complete.");

    metrics::print_metrics_summary();

    println!("\n💡 Measure throughput on your machine: cargo bench");

    println!("\n🔍 Prometheus Metrics Format:");
    println!("==============================");
    println!("{}", metrics::get_metrics());
}

fn show(i: u32, allowed: bool) {
    let label = if allowed {
        "✅ ALLOWED"
    } else {
        "❌ BLOCKED"
    };
    println!("   Request {i}: {label}");
}

async fn run_algorithm(limiter: &dyn RateLimiter) {
    for i in 1..=5 {
        let start = Instant::now();
        let allowed = limiter.check("user_test", Cost::ONE).allowed;
        record_request(allowed, start);
        show(i, allowed);
    }

    println!("   ⏳ Waiting 1 second...");
    tokio::time::sleep(Duration::from_secs(1)).await;

    for i in 1..=2 {
        let start = Instant::now();
        let allowed = limiter.check("user_test", Cost::ONE).allowed;
        record_request(allowed, start);
        show(i, allowed);
    }
}

async fn run_redis(config: RateLimitConfig) {
    let limiter = match RedisRateLimiter::connect("redis://127.0.0.1/", config).await {
        Ok(l) => l,
        Err(e) => {
            println!("   ⚠️  Redis not available ({e})");
            println!("   💡 Start one with: docker run --rm -p 6379:6379 redis:7.4-alpine");
            return;
        }
    };
    if limiter.reset("user_redis").await.is_err() {
        println!("   ⚠️  Redis did not answer");
        return;
    }
    println!("   ✅ Connected to Redis\n");

    for i in 1..=5 {
        let start = Instant::now();
        match limiter.check("user_redis", Cost::ONE).await {
            Ok(decision) => {
                record_request(decision.allowed, start);
                show(i, decision.allowed);
            }
            Err(_) => {
                println!("   ⚠️  Redis connection lost during test");
                break;
            }
        }
    }
    let _ = limiter.reset("user_redis").await;
}
