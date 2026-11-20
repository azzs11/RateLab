# Distributed Rate Limiter

<div align="center">

[![Rust CI](https://github.com/azzs11/distributed-rate-limiter/actions/workflows/rust.yml/badge.svg)](https://github.com/azzs11/distributed-rate-limiter/actions/workflows/rust.yml)
![License](https://img.shields.io/badge/License-MIT-blue)
![Stars](https://img.shields.io/github/stars/azzs11/distributed-rate-limiter?style=social)

**Rate limiting algorithms in Rust (token bucket, leaky bucket, fixed and sliding window, GCRA), shared across nodes through Redis, with a benchmark that reproduces every number below.**

[Features](#features) • [Quick Start](#quick-start) • [Benchmarks](#benchmarks) • [Roadmap](#roadmap)

</div>

---

## Overview

Five rate limiting algorithms behind one `RateLimiter` trait, with thread-safe
state and injected time; a Redis store that several nodes share, behind a circuit
breaker; an Axum server whose Tower layer sends IETF `RateLimit` headers; and a
benchmark that measures accuracy against an exact oracle, on one machine and on a
3-node Docker cluster.

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

### Shared through Redis (`drl-redis-store`)

`RedisRateLimiter` keeps a token bucket per key in Redis, so every node that talks
to the same Redis enforces one shared limit.

- **One multiplexed connection** per limiter, reconnecting automatically.
- **EVALSHA:** the Lua script ([`scripts/token_bucket.lua`](crates/redis-store/scripts/token_bucket.lua))
  is sent by hash; the full script only goes over the wire again after `NOSCRIPT`.
- **Redis `TIME`:** the script reads Redis's clock, not the application server's,
  so nodes with skewed clocks still agree.
- **Exact integers:** token amounts are scaled by the window in microseconds and
  kept below 2^53, so Lua's doubles never round.
- **Circuit breaker:** `GuardedLimiter` wraps the store in a closed / open /
  half-open breaker (`drl_core::CircuitBreaker`). While Redis is down, each
  call's `FailurePolicy` decides: **fail open** (allow) or **fail closed** (deny).
- **Tested against a real Redis** started by testcontainers, including an outage.

### HTTP server (`crates/server`)

An Axum app with a Tower `RateLimitLayer` on real routes:

| Route | Policy | When Redis is down |
|-------|--------|--------------------|
| `GET /api/hello` | `public` | fails open |
| `POST /api/orders` | `orders` | fails closed |
| `GET /health`, `GET /metrics` | not limited | |

Every limited response carries the IETF draft headers
([draft-ietf-httpapi-ratelimit-headers](https://datatracker.ietf.org/doc/draft-ietf-httpapi-ratelimit-headers/)):

```text
RateLimit-Policy: "public";q=100;w=1
RateLimit: "public";r=99;t=1
Retry-After: 1                       (on 429 only)
x-drl-source: redis                  (local | redis | breaker_open | redis_error)
```

Callers are identified by the `x-api-key` header, else their IP. Time values are
rounded **up** to whole seconds, so a client that waits `Retry-After` never retries
too early. `/metrics` exports Prometheus counters and a decision-time histogram
(through the `metrics` crate).

### Other

- **Web dashboard:** run a burst of requests against any algorithm and see allowed/blocked results
- **Simulator** (`crates/sim`): seeded, deterministic traffic (steady Poisson, on/off,
  heavy-tailed Pareto, diurnal ramp, flash crowd, the fixed-window boundary attack)
  replayed through any limiter, identical on every platform
- **WebAssembly bindings** (`crates/wasm`): the same algorithms, compiled for the
  browser, for the upcoming playground

### Workspace layout

| Crate | What it holds |
|-------|---------------|
| `crates/core` (`drl-core`) | Algorithms, `Clock`, `Decision`, circuit breaker, unit newtypes. No I/O. |
| `crates/redis-store` (`drl-redis-store`) | Redis token bucket (Lua, EVALSHA, Redis `TIME`) and `GuardedLimiter` |
| `crates/server` | Axum server with the Tower layer and dashboard (`web-server`), demo binary (`demo`) |
| `crates/sim` | Deterministic traffic scenarios |
| `crates/wasm` | wasm-bindgen wrapper for the browser |
| `crates/bench` | Exact oracle, measurement suite, load generator, RESULTS.md writer, Criterion benches |
| `deploy/` | Dockerfile and Docker Compose: 3 nodes, Redis, load generator |

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

The dashboard runs bursts against a fresh in-process limiter; the limited routes
above use the server's configured backend.

---

## Quick Start

### Prerequisites

- Rust 1.89 or newer via [rustup](https://rustup.rs/) (the minimum is checked in CI)
- Docker (optional): for the Redis tests and the 3-node cluster
- [just](https://github.com/casey/just) (optional): `cargo install just --locked`

### Installation

```bash
git clone https://github.com/azzs11/distributed-rate-limiter.git
cd distributed-rate-limiter

cargo build --release
cargo test --workspace

cargo run -p server --bin demo        # terminal demo of every algorithm
cargo run -p server --bin web-server  # server and dashboard on http://localhost:3001
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

### Shared through Redis

```rust
use drl_core::{BreakerConfig, CircuitBreaker, Cost, FailurePolicy, MonotonicClock, RateLimitConfig};
use drl_redis_store::{GuardedLimiter, RedisRateLimiter};
use std::time::Duration;

let config = RateLimitConfig::per_second(1000)?;
let store = RedisRateLimiter::connect("redis://127.0.0.1/", config).await?;
let breaker = CircuitBreaker::new(BreakerConfig::new(5, Duration::from_secs(1))?, MonotonicClock::new());
let limiter = GuardedLimiter::new(store, breaker);

let checked = limiter.check("global_api_key", Cost::ONE, FailurePolicy::FailClosed).await;
println!("allowed: {} (decided by {:?})", checked.decision.allowed, checked.source);
```

### Server with Redis

```bash
docker run --rm -p 6379:6379 redis:7.4-alpine
cargo run -p server --bin web-server -- --backend redis --limit 100 --window-ms 1000

curl -i -H 'x-api-key: alice' localhost:3001/api/hello
```

Every flag can also be set by environment variable (`DRL_BACKEND`, `DRL_LIMIT`, ...);
`--help` lists them.

### Three nodes in Docker

```bash
just cluster           # or see deploy/docker-compose.yml
```

---

## Benchmarks

<!-- results:start (generated by `just results`, do not edit) -->

Measured on Apple M5, 10 logical CPUs, 16 GiB RAM, macOS 27.0 (aarch64), rustc 1.100.0 (0ef1bc386 2026-11-09), release build (commit `d47ebdde8de9`, 2026-11-20T07:06:06Z). Full tables, spreads and method: [results/RESULTS.md](results/RESULTS.md).

| Algorithm | Worst window, boundary attack (× limit) | Over-admission, steady Poisson % | p50 latency ns | 1 thread, 1000 keys | Bytes/key, full window |
|---|---:|---:|---:|---:|---:|
| token_bucket | 1.00 | 20.97 | 45.0 | 25.02 M/s | 96 |
| leaky_bucket | 1.00 | 20.97 | 45.2 | 24.20 M/s | 96 |
| fixed_window | 2.00 | 23.81 | 37.7 | 27.61 M/s | 65 |
| sliding_window | 1.00 | 0.00 | 54.5 | 22.81 M/s | 2144 |
| gcra | 1.00 | 20.97 | 45.8 | 25.61 M/s | 75 |

**3 nodes sharing Redis** (Docker Compose on the same machine): over-admission 1.64% compared with one perfectly coordinated token bucket, 1000 Redis calls per 1,000 decisions, p99 1.23 ms at the client, 123476 requests/s closed-loop.

<!-- results:end -->

### Reproduce

```bash
just results
```

One command runs everything and rewrites [results/RESULTS.md](results/RESULTS.md)
and the table above:

1. `cargo run -p bench --release -- local`: in-process accuracy, latency,
   throughput and fairness (`results/local.json`);
2. `cargo run -p bench --release --bin memory`: bytes per key (`results/memory.json`);
3. `just cluster`: 3 nodes + Redis + load generator in Docker Compose
   (`results/cluster.json`);
4. `cargo run -p bench --release -- report`: the Markdown, from the JSON files only.

**Method.**

- **Accuracy:** measured against an exact oracle, a plain log per key that admits a
  request only if the key stays within its limit in every window-long span. The
  oracle shares no code with the library.
- **Scenarios:** seeded, so accuracy numbers are reproducible bit for bit.
- **Latency and throughput:** warmed up, then repeated; the tables show mean and
  min–max.
- **Machine:** every result file records the machine, OS, rustc and commit it was
  measured on.

**Limits of these numbers.**

- **Cluster run:** the nodes, Redis and the load generator share one machine and
  Docker's network, so the cluster numbers show the protocol's behaviour, not a
  production deployment.
- **Latency on macOS:** the clock ticks too coarsely to time one call, so latency is
  timed in batches of calls (see RESULTS.md).

Criterion micro-benchmarks for regression checks: `cargo bench -p bench`.

---

## Testing

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# Starts Redis containers with testcontainers (needs Docker; CI runs these on every push)
cargo test --workspace -- --ignored
```

No test sleeps: time-dependent tests drive a `ManualClock`, and the Redis outage
test pauses Redis with `CLIENT PAUSE` instead of waiting.

---

## Roadmap

These features are **not built yet**:

- **Coordination strategies:** local split, async sync and adaptive quota leasing,
  compared with Redis-per-call on the same benchmark (Phase 3)
- **Multi-tier limits:** per user, IP, endpoint and global limits combined
- **WebSocket support:** live dashboard updates

The full phase plan is in [docs/ROADMAP.md](docs/ROADMAP.md).

---

## Tech Stack

- **Rust** (2024 edition): library, server, benchmark
- **DashMap**: thread-safe per-key state
- **Redis** (`redis` crate, Lua scripting): shared state across nodes
- **Axum** and **Tower**: HTTP server and rate limit middleware
- **metrics** with the Prometheus exporter: metrics
- **testcontainers** and **Docker Compose**: real Redis in tests, 3-node cluster
- **wasm-bindgen**: WebAssembly bindings
- **Tailwind CSS** and **Chart.js** (via CDN): dashboard UI

---

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-change`)
3. Make sure `just test` passes (or `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all -- --check`)
4. Open a pull request

---

## License

MIT License. See [LICENSE](LICENSE).

---

## Author

**Akchhya Singh**

- GitHub: [@azzs11](https://github.com/azzs11)
- LinkedIn: [akchhya-singh11](https://linkedin.com/in/akchhya-singh11)
