# Ratelab

**Distributed rate limiting, measured.** A Rust rate limiter that grades itself against
an exact oracle, on simulated, real-Redis and real-traffic runs.

- Adaptive quota leasing: {{s3.adaptive_leasing.steady_poisson.saved_pct}}% fewer Redis
  calls than exact central limiting at {{s3.adaptive_leasing.steady_poisson.err}}%
  admission error (3 nodes); simulator matched a real Redis cluster to within
  {{realcheck.err_gap_pp}} points.
- Overload: a Gradient2 concurrency limit kept {{ov.ramp.gradient2.goodput}}% of
  possible goodput under a {{ov.ramp.peak_x}}× load ramp, against
  {{ov.ramp.none.goodput}}% with no limit.
- LLM tokens: admit-then-reconcile turned away {{llm.admit_then_reconcile.wrong}}% of
  requests on {{llm.requests}} real Azure requests, against {{llm.reserve_max.wrong}}% for
  reserving the maximum.

**Stack:** Rust, Tokio, Axum, Tower, Redis (Lua), WebAssembly, TypeScript, Docker.

**Links:** [Playground](https://azzs11.github.io/distributed-rate-limiter/) ·
[Code](https://github.com/azzs11/distributed-rate-limiter) ·
[crates.io](https://crates.io/crates/ratelab) ·
[Report](https://github.com/azzs11/distributed-rate-limiter/blob/main/docs/report/the-price-of-exactness.pdf)
