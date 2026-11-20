# Distributed Rate Limiter

<div align="center">

[![Rust CI](https://github.com/azzs11/distributed-rate-limiter/actions/workflows/rust.yml/badge.svg)](https://github.com/azzs11/distributed-rate-limiter/actions/workflows/rust.yml)
![License](https://img.shields.io/badge/License-MIT-blue)
![Stars](https://img.shields.io/github/stars/azzs11/distributed-rate-limiter?style=social)

**Rate limiting algorithms in Rust (token bucket, leaky bucket, fixed and sliding window, GCRA) with a Redis-backed prototype.**

[Features](#features) • [Quick Start](#quick-start) • [Benchmarks](#benchmarks) • [Roadmap](#roadmap)

</div>

---

## Overview

Five rate limiting algorithms behind one `RateLimiter` trait, with thread-safe
state and injected time, plus Prometheus metrics, a small Actix web dashboard for
trying the algorithms interactively, and a Redis-backed token bucket prototype.

The project is being reworked into a rate limiter that **measures and reports
its own accuracy**. See [docs/ROADMAP.md](docs/ROADMAP.md) for the plan.

---

## Features

### Algorithms (in-process, `drl-core`)

| Algorithm | How it works | Trade-off |
|-----------|--------------|-----------|
| **Token Bucket** | Tokens refill at a constant rate; each request spends its cost | Allows bursts up to the bucket size |
| **Leaky Bucket** | Requests fill a bucket that drains at a constant rate | Smooths bursts; bursty traffic gets rejected |
| **Fixed Window** | Counts usage per fixed time window | Simplest; can admit a burst above the limit around a window boundary |
| **Sliding Window** | Stores each admitted request in the window | Exact limit, but memory grows with the limit |
| **GCRA** | Tracks one theoretical arrival time per key | Same decisions as the token bucket, one number of state per key |

How they behave:

- **One API:** `check(&self, key, cost)` returns a `Decision` with `allowed`,
  `remaining`, `reset_after` and `retry_after`.
- **Thread-safe:** limiters take `&self` and keep per-key state in a `DashMap`, so one
  instance can be shared across threads. State is **not** shared between processes.
- **Injected time:** algorithms read time from a `Clock`. Tests use `ManualClock`
  and never sleep.
- **Exact integer arithmetic:** the same inputs give the same decisions on every
  platform, including WebAssembly.
- **Shared conformance suite:** every algorithm must pass the same tests
  (`crates/core/tests/conformance.rs`), including an exact `retry_after` and a
  multi-threaded no-over-admission check.

### Redis prototype

`RedisRateLimiter` (`crates/redis-store`) runs a token bucket as an atomic
Lua script in Redis, so several processes could share one limit. It is a
**prototype**:

- The web server does **not** use it yet. The dashboard only runs the
  in-process algorithms.
- It opens a new Redis connection on every request.
- It uses the application server's clock, not Redis `TIME`.
- `check_with_fallback` fails open (allows the request) when Redis is
  unreachable. That is a fallback, not a circuit breaker.

### Other

- **Prometheus metrics:** request counters and a latency histogram (`crates/server/src/metrics.rs`)
- **Web dashboard:** run a burst of requests against any algorithm and see allowed/blocked results
- **Simulator** (`crates/sim`): seeded, deterministic traffic (jittered, bursty, the
  fixed-window boundary attack) replayed through any limiter
- **WebAssembly bindings** (`crates/wasm`): the same algorithms, compiled for the
  browser, for the upcoming playground

### Workspace layout

| Crate | What it holds |
|-------|---------------|
| `crates/core` (`drl-core`) | Algorithms, `Clock`, `Decision`, unit newtypes. No I/O. |
| `crates/redis-store` (`drl-redis-store`) | Redis-backed token bucket prototype |
| `crates/server` | Actix web dashboard (`web-server`) and demo binary (`demo`) |
| `crates/sim` | Deterministic traffic simulation |
| `crates/wasm` | wasm-bindgen wrapper for the browser |
| `crates/bench` | Criterion benchmarks and measurement scenarios |

---

## Demo

### Web dashboard

<p align="center">
  <img src="docs/screenshots/Metrics_1.png" width="45%" />
  <img src="docs/screenshots/Metrics_graph_1.png" width="45%" />
</p>

<p align="center"><em>Screenshots of one local run. The values come from the dashboard on that machine; they are not benchmark results.</em></p>

### Algorithm guide

<p align="center">
  <img src="docs/screenshots/Alg_1.png" width="70%" />
</p>

```bash
cargo run -p server --bin web-server
# then open http://localhost:3001
```

---

## Quick Start

### Prerequisites

- Rust 1.89 or newer via [rustup](https://rustup.rs/) (the minimum is checked in CI)
- Redis (optional, only for the Redis prototype)

### Installation

```bash
git clone https://github.com/azzs11/distributed-rate-limiter.git
cd distributed-rate-limiter

cargo build --release
cargo test --workspace

cargo run -p server --bin demo        # terminal demo of every algorithm
cargo run -p server --bin web-server  # dashboard on http://localhost:3001
```

### Basic usage

```rust
use drl_core::{Cost, RateLimitConfig, RateLimiter, TokenBucket};

// 100 requests per second
let config = RateLimitConfig::per_second(100)?;
let limiter = TokenBucket::new(config);

let decision = limiter.check("user_123", Cost::ONE);
if decision.allowed {
    println!("Request allowed, {} left", decision.remaining.0);
} else {
    println!("Rate limited, retry in {:?}", decision.retry_after);
}
```

`limiter` takes `&self`, so you can wrap it in an `Arc` and share it across threads.
Pick an algorithm at runtime with `"gcra".parse::<Algorithm>()?.build(config, clock)`.

### Testing with a manual clock

```rust
use drl_core::{Cost, Gcra, ManualClock, RateLimitConfig, RateLimiter};

let clock = ManualClock::new();
let limiter = Gcra::with_clock(RateLimitConfig::per_second(2)?, &clock);

assert!(limiter.check("k", Cost(2)).allowed);
let denied = limiter.check("k", Cost::ONE);
assert!(!denied.allowed);

clock.advance(denied.retry_after.unwrap()); // no sleeping
assert!(limiter.check("k", Cost::ONE).allowed);
```

### Redis prototype

```rust
use drl_core::{Cost, RateLimitConfig};
use drl_redis_store::RedisRateLimiter;

let config = RateLimitConfig::per_second(1000)?;
let limiter = RedisRateLimiter::new("redis://127.0.0.1/", config)?;

let decision = limiter.check("global_api_key", Cost::ONE)?;
```

---

## Benchmarks

This repo does **not** publish benchmark numbers yet. A reproducible benchmark
that measures accuracy as well as throughput is planned for Phase 2 of the
[roadmap](docs/ROADMAP.md).

You can measure on your own machine:

```bash
# Criterion micro-benchmarks (HTML report in target/criterion/report/index.html)
cargo bench -p bench

# Measurement scenarios, printing what this machine measures
cargo run -p bench --release -- load
cargo run -p bench --release -- latency
```

---

## Testing

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# Needs a Redis server on 127.0.0.1:6379 (CI runs this against a Redis container)
cargo test -p drl-redis-store -- --ignored
```

No test sleeps: time-dependent tests drive a `ManualClock`.

---

## Roadmap

These features are **not built yet**:

- **Multi-tier limits:** per user, IP, endpoint and global limits combined
- **Circuit breaker:** real open/half-open/closed states around the Redis backend
- **WebSocket support:** live dashboard updates
- **Server uses Redis:** the dashboard/server backed by the distributed limiter

The full phase plan is in [docs/ROADMAP.md](docs/ROADMAP.md).

---

## Tech Stack

- **Rust** (2024 edition): library, demo binary and web server
- **DashMap**: thread-safe per-key state
- **Redis**: distributed prototype (Lua scripting)
- **wasm-bindgen**: WebAssembly bindings
- **Prometheus** (`prometheus` crate): metrics
- **Actix-web**: HTTP server
- **Tailwind CSS** and **Chart.js** (via CDN): dashboard UI

---

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-change`)
3. Make sure `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check` pass
4. Open a pull request

---

## License

MIT License. See [LICENSE](LICENSE).

---

## Author

**Akchhya Singh**

- GitHub: [@azzs11](https://github.com/azzs11)
- LinkedIn: [akchhya-singh11](https://linkedin.com/in/akchhya-singh11)
