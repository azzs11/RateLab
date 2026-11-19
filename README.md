# Distributed Rate Limiter

<div align="center">

[![Rust CI](https://github.com/azzs11/distributed-rate-limiter/actions/workflows/rust.yml/badge.svg)](https://github.com/azzs11/distributed-rate-limiter/actions/workflows/rust.yml)
![License](https://img.shields.io/badge/License-MIT-blue)
![Stars](https://img.shields.io/github/stars/azzs11/distributed-rate-limiter?style=social)

**Rate limiting algorithms in Rust (token bucket, leaky bucket, fixed and sliding window) with a Redis-backed prototype.**

[Features](#features) • [Quick Start](#quick-start) • [Benchmarks](#benchmarks) • [Roadmap](#roadmap)

</div>

---

## Overview

This crate implements four classic rate limiting algorithms behind one
`RateLimiter` trait. It also includes Prometheus metrics, a small Actix web
dashboard for trying the algorithms interactively, and a Redis-backed token
bucket prototype.

The project is being reworked into a rate limiter that **measures and reports
its own accuracy**. See [docs/ROADMAP.md](docs/ROADMAP.md) for the plan.

---

## Features

### Algorithms (in-process)

| Algorithm | How it works | Trade-off |
|-----------|--------------|-----------|
| **Token Bucket** | Tokens refill at a constant rate; each request spends one | Allows bursts up to the bucket size |
| **Leaky Bucket** | Requests fill a bucket that drains at a constant rate | Smooths bursts; bursty traffic gets rejected |
| **Fixed Window** | Counts requests per fixed time window | Simplest; can admit a burst above the limit around a window boundary |
| **Sliding Window** | Stores each request timestamp in the window | Exact limit, but memory grows with the limit |

These limiters keep state in a per-instance `HashMap`. They are **not**
shared between processes.

### Redis prototype

`RedisRateLimiter` (`src/redis_limiter.rs`) runs a token bucket as an atomic
Lua script in Redis, so several processes could share one limit. It is a
**prototype**:

- The web server does **not** use it yet. The dashboard only runs the
  in-process algorithms.
- It opens a new Redis connection on every request.
- `check_with_fallback` fails open (allows the request) when Redis is
  unreachable. That is a fallback, not a circuit breaker.

### Other

- **Prometheus metrics:** request counters and a latency histogram (`src/metrics.rs`)
- **Web dashboard:** run a burst of requests against any algorithm and see allowed/blocked results

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
cargo run --bin web-server
# then open http://localhost:3001
```

---

## Quick Start

### Prerequisites

- Rust (stable) via [rustup](https://rustup.rs/)
- Redis (optional, only for the Redis prototype)

### Installation

```bash
git clone https://github.com/azzs11/distributed-rate-limiter.git
cd distributed-rate-limiter

cargo build --release
cargo test
```

### Basic usage

```rust
use distributed_rate_limiter::algorithms::TokenBucket;
use distributed_rate_limiter::{RateLimitConfig, RateLimiter};

// 100 requests per second
let config = RateLimitConfig::per_second(100);
let mut limiter = TokenBucket::new(config);

if limiter.allow_request("user_123").unwrap() {
    println!("Request allowed");
} else {
    println!("Rate limit exceeded");
}
```

### Redis prototype

```rust
use distributed_rate_limiter::redis_limiter::RedisRateLimiter;
use distributed_rate_limiter::{RateLimitConfig, RateLimiter};

let config = RateLimitConfig::per_second(1000);
let mut limiter = RedisRateLimiter::new("redis://127.0.0.1/", config)?;

limiter.allow_request("global_api_key")?;
```

### With metrics

```rust
use distributed_rate_limiter::metrics::{self, record_request};
use std::time::Instant;

metrics::init_metrics();

let start = Instant::now();
let allowed = limiter.allow_request("user")?;
record_request(allowed, start);

// Prometheus text format
println!("{}", metrics::get_metrics());
```

---

## Benchmarks

This repo does **not** publish benchmark numbers yet. A reproducible benchmark
that measures accuracy as well as throughput is planned for Phase 2 of the
[roadmap](docs/ROADMAP.md).

You can measure on your own machine:

```bash
# Criterion micro-benchmarks (HTML report in target/criterion/report/index.html)
cargo bench

# Load tests, printing the throughput measured on this machine
cargo test --release --test load_test -- --nocapture
```

---

## Testing

```bash
cargo test                          # unit + integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

The Redis test passes without a running Redis server: it skips itself when it
cannot connect.

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

- **Rust**: library, demo binary and web server
- **Redis**: distributed prototype (Lua scripting)
- **Prometheus** (`prometheus` crate): metrics
- **Actix-web**: HTTP server
- **Tailwind CSS** and **Chart.js** (via CDN): dashboard UI

---

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-change`)
3. Make sure `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt -- --check` pass
4. Open a pull request

---

## License

MIT License. See [LICENSE](LICENSE).

---

## Author

**Akchhya Singh**

- GitHub: [@azzs11](https://github.com/azzs11)
- LinkedIn: [akchhya-singh11](https://linkedin.com/in/akchhya-singh11)
