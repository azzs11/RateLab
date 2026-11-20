//! Reproducible measurements for the rate limiter.
//!
//! Every number in RESULTS.md and the README comes from here:
//!
//! - [`local`]: in-process accuracy against the exact [`oracle`], latency,
//!   throughput and fairness;
//! - [`memory`]: bytes per key (the `memory` binary);
//! - [`cluster`]: the 3-node Docker run against Redis (the `loadgen` binary);
//! - [`strategies`]: the five coordination strategies in the multi-node simulator;
//! - [`real_strategies`]: the strategies on the real Docker cluster, against the simulator;
//! - [`llm`]: token limits for LLM traffic (estimate-only, reserve-max,
//!   admit-then-reconcile) on a real Azure trace and synthetic requests;
//! - [`overload`]: no limit, static, AIMD and Gradient2 concurrency limits in front of a
//!   backend with limited capacity, simulated and on the real Tower layer;
//! - [`real_traffic`]: every algorithm and strategy on a public request log ([`traces`]);
//! - [`report`]: turns `results/*.json` into RESULTS.md and the README table, with the
//!   [`machine`] each file was measured on.

#![forbid(unsafe_code)]

pub mod cluster;
pub mod llm;
pub mod local;
pub mod machine;
pub mod memory;
pub mod oracle;
pub mod overload;
pub mod real_strategies;
pub mod real_traffic;
pub mod report;
pub mod results;
pub mod stats;
pub mod strategies;
pub mod svg;
pub mod traces;
