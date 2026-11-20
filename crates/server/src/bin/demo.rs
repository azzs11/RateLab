use drl_redis_store::RedisRateLimiter;
use ratelab::{Algorithm, Cost, MonotonicClock, RateLimitConfig, RateLimiter};
use server::metrics::Stats;
use std::sync::LazyLock;
use std::time::Duration;

/// Totals across every section of the demo.
static STATS: LazyLock<Stats> = LazyLock::new(Stats::default);

#[tokio::main]
async fn main() {
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

    let (allowed, blocked) = STATS.totals();
    println!("\nSUMMARY");
    println!("=======");
    println!("Allowed: {allowed}");
    println!("Blocked: {blocked}");

    println!("\n💡 Measure on your machine: cargo run -p bench --release -- local");
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
        let allowed = limiter.check("user_test", Cost::ONE).allowed;
        STATS.count(allowed);
        show(i, allowed);
    }

    println!("   ⏳ Waiting 1 second...");
    tokio::time::sleep(Duration::from_secs(1)).await;

    for i in 1..=2 {
        let allowed = limiter.check("user_test", Cost::ONE).allowed;
        STATS.count(allowed);
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
        match limiter.check("user_redis", Cost::ONE).await {
            Ok(decision) => {
                STATS.count(decision.allowed);
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
