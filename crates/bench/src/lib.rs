//! Reproducible measurements for the rate limiter.
//!
//! Every number in RESULTS.md and the README comes from here:
//!
//! - [`local`]: in-process accuracy against the exact [`oracle`], latency,
//!   throughput and fairness;
//! - [`memory`]: bytes per key (the `memory` binary);
//! - [`report`]: turns `results/*.json` into RESULTS.md and the README table, with the
//!   [`machine`] each file was measured on.

#![forbid(unsafe_code)]

pub mod local;
pub mod machine;
pub mod memory;
pub mod oracle;
pub mod report;
pub mod results;
pub mod stats;
