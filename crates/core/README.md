# ratelab

Rate limiting for Rust services, with the measurements to choose between approaches:
five single-node algorithms, five ways for a cluster to share one limit, adaptive
concurrency limits for overload, and token budgets for LLM traffic. No I/O, no
`unsafe`, exact integer arithmetic, and time you can control in tests.

```toml
[dependencies]
ratelab = "0.1"
```

## A limit per key

Every algorithm implements [`RateLimiter`]: `check(&self, key, cost)` returns a
[`Decision`] with `allowed`, `remaining`, `reset_after` and `retry_after`. Limiters take
`&self` and keep per-key state in sharded maps, so one instance can be shared across
threads in an `Arc`.

```rust
use ratelab::{Cost, Gcra, RateLimitConfig, RateLimiter};

let limiter = Gcra::new(RateLimitConfig::per_second(100)?);
let d = limiter.check("user_42", Cost::ONE);
if !d.allowed {
    println!("retry in {:?}", d.retry_after);
}
# Ok::<(), ratelab::ConfigError>(())
```

| Algorithm | Remembers per key | Exact? |
|---|---|---|
| [`TokenBucket`] | a level and a timestamp | allows a full burst on top of a window's refill |
| [`LeakyBucket`] | a level and a timestamp | the token bucket's decisions, seen as water |
| [`FixedWindow`] | a counter and its window | up to 2× the limit across a window edge |
| [`SlidingWindow`] | every admitted request in the last window | exact; memory grows with the limit |
| [`Gcra`] | one timestamp | the token bucket's decisions, one number of state |

## Time you control

Time comes from a [`Clock`]. Tests use [`ManualClock`] and never sleep:

```rust
use ratelab::{Cost, ManualClock, RateLimitConfig, RateLimiter, TokenBucket};

let clock = ManualClock::new();
let limiter = TokenBucket::with_clock(RateLimitConfig::per_second(2)?, &clock);
assert!(limiter.check("alice", Cost::ONE).allowed);
assert!(limiter.check("alice", Cost::ONE).allowed);
let denied = limiter.check("alice", Cost::ONE);
assert!(!denied.allowed);

// Wait exactly as long as the limiter says, then retry.
clock.advance(denied.retry_after.unwrap());
assert!(limiter.check("alice", Cost::ONE).allowed);
# Ok::<(), ratelab::ConfigError>(())
```

## Beyond one counter

- **[`concurrency`]:** limits on requests *in flight*, static, AIMD or Gradient2, which
  shed overload with a fast refusal instead of letting queues grow past every
  deadline.
- **[`tokens`]:** input and output token budgets for LLM traffic, with three ways to
  charge for an answer that has not been written yet: estimate only, reserve the
  maximum, or admit then reconcile (carrying any shortfall as debt).
- **[`strategies`]:** central exact, local split, local with async sync, fixed and
  adaptive quota leasing: the protocols a cluster can use to share one limit, as pure
  state machines you drive with your own store and network.
- **[`CircuitBreaker`]** with a [`FailurePolicy`] (fail open or closed) for when a
  shared store is unreachable.

```rust
use ratelab::concurrency::{Completion, Concurrency, ConcurrencyLimiter, Gradient2Config};
use ratelab::Nanos;
use std::time::Duration;

let limiter = ConcurrencyLimiter::new(Concurrency::Gradient2(Gradient2Config::default()));
if let Some(ticket) = limiter.try_acquire() {
    // ... handle the request, then report how it went:
    limiter.release(ticket, Nanos(0), Duration::from_millis(12), Completion::Success);
} else {
    // over the limit: answer 503 at once
}
```

## Features

- `std-clock` (default): [`MonotonicClock`], the real clock. Turn default features off
  for `wasm32-unknown-unknown`, where time is passed in by the host.

The minimum supported Rust version is 1.89.

## Measured, not claimed

Every claim about accuracy and cost above is measured by the project's reproducible
benchmark, including real traffic, a real Redis cluster and a real LLM trace. Results,
method and an interactive playground that runs this crate in the browser:

- Results: <https://github.com/azzs11/distributed-rate-limiter/blob/main/results/RESULTS.md>
- Playground: <https://azzs11.github.io/distributed-rate-limiter/>
- Source: <https://github.com/azzs11/distributed-rate-limiter>

Licensed under MIT.

[`RateLimiter`]: https://docs.rs/ratelab/latest/ratelab/trait.RateLimiter.html
[`Decision`]: https://docs.rs/ratelab/latest/ratelab/decision/struct.Decision.html
[`TokenBucket`]: https://docs.rs/ratelab/latest/ratelab/algorithms/struct.TokenBucket.html
[`LeakyBucket`]: https://docs.rs/ratelab/latest/ratelab/algorithms/struct.LeakyBucket.html
[`FixedWindow`]: https://docs.rs/ratelab/latest/ratelab/algorithms/struct.FixedWindow.html
[`SlidingWindow`]: https://docs.rs/ratelab/latest/ratelab/algorithms/struct.SlidingWindow.html
[`Gcra`]: https://docs.rs/ratelab/latest/ratelab/algorithms/struct.Gcra.html
[`Clock`]: https://docs.rs/ratelab/latest/ratelab/clock/trait.Clock.html
[`ManualClock`]: https://docs.rs/ratelab/latest/ratelab/clock/struct.ManualClock.html
[`MonotonicClock`]: https://docs.rs/ratelab/latest/ratelab/clock/struct.MonotonicClock.html
[`concurrency`]: https://docs.rs/ratelab/latest/ratelab/concurrency/index.html
[`tokens`]: https://docs.rs/ratelab/latest/ratelab/tokens/index.html
[`strategies`]: https://docs.rs/ratelab/latest/ratelab/strategies/index.html
[`CircuitBreaker`]: https://docs.rs/ratelab/latest/ratelab/breaker/struct.CircuitBreaker.html
[`FailurePolicy`]: https://docs.rs/ratelab/latest/ratelab/breaker/enum.FailurePolicy.html
