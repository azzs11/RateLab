# Changelog

All notable changes to the `ratelab` crate. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the crate follows
[Semantic Versioning](https://semver.org/); `cargo semver-checks` runs before each
release.

## 0.1.0

First release.

- Five algorithms behind one `RateLimiter` trait: token bucket, leaky bucket, fixed
  window, sliding window log and GCRA, with thread-safe per-key state.
- Injected time (`Clock`, `ManualClock`, `MonotonicClock`) and exact integer arithmetic.
- `CircuitBreaker` with fail-open or fail-closed `FailurePolicy`.
- `strategies`: central exact, local split, local with async sync, fixed and adaptive
  quota leasing, as I/O-free state machines, plus an in-memory `Store`.
- `concurrency`: static, AIMD and Gradient2 concurrency limits.
- `tokens`: input and output token budgets with per-model weights, and estimate-only,
  reserve-max and admit-then-reconcile accounting.
