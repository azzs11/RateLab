**Ratelab: distributed rate limiter** (Rust, Redis, Axum, WebAssembly) ·
github.com/azzs11/distributed-rate-limiter · crates.io/crates/ratelab

- Built and published a Rust rate-limiting library with 5 algorithms and 5 cluster
  coordination strategies, benchmarked against an exact oracle on simulated, real-Redis
  and real-traffic runs ({{trace.requests}}-request public log); every reported number is
  generated from reproducible result files.
- Designed adaptive quota leasing: {{s3.adaptive_leasing.steady_poisson.saved_pct}}% fewer
  Redis calls than exact central limiting at {{s3.adaptive_leasing.steady_poisson.err}}%
  admission error on 3 nodes; validated the simulator against a real 3-node Redis
  cluster to within {{realcheck.err_gap_pp}} percentage points.
- Added Gradient2 concurrency limits ({{ov.ramp.gradient2.goodput}}% vs
  {{ov.ramp.none.goodput}}% goodput under {{ov.ramp.peak_x}}× overload) and LLM token
  budgets with admit-then-reconcile accounting ({{llm.admit_then_reconcile.wrong}}% vs
  {{llm.reserve_max.wrong}}% of requests wrongly refused on {{llm.requests}} real Azure LLM
  requests).
