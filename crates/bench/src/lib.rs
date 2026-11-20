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
//! - [`real_traffic`]: every algorithm and strategy on a public request log ([`traces`]);
//! - [`report`]: turns `results/*.json` into RESULTS.md and the README table, with the
//!   [`machine`] each file was measured on.

#![forbid(unsafe_code)]

pub mod cluster;
pub mod local;
pub mod machine;
pub mod memory;
pub mod oracle;
pub mod real_strategies;
pub mod real_traffic;
pub mod report;
pub mod results;
pub mod stats;
pub mod strategies;
pub mod svg;
pub mod traces;
