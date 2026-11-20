//! Result files written to `results/*.json`. RESULTS.md is generated from these and
//! never edited by hand.

use crate::local::{AccuracyRow, FairnessRow, LatencyRow, SuiteSize, ThroughputRow, TimerInfo};
use crate::machine::Machine;
use crate::memory::MemoryRow;
use anyhow::Context;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Version of the result file format.
pub const SCHEMA: u32 = 1;

/// `results/local.json`: every in-process measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalResults {
    /// File format version.
    pub schema: u32,
    /// Where it ran.
    pub machine: Machine,
    /// How much work each measurement did.
    pub size: SuiteSize,
    /// The clock latency samples are measured with.
    pub timer: TimerInfo,
    /// Accuracy against the oracle.
    pub accuracy: Vec<AccuracyRow>,
    /// Decision latency.
    pub latency: Vec<LatencyRow>,
    /// Throughput.
    pub throughput: Vec<ThroughputRow>,
    /// Fairness between tenants.
    pub fairness: Vec<FairnessRow>,
}

/// `results/memory.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryResults {
    /// Where it ran.
    pub machine: Machine,
    /// One row per algorithm.
    pub rows: Vec<MemoryRow>,
}

/// Writes `value` as pretty JSON, creating the parent directory.
pub fn write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(value)?;
    std::fs::write(path, json + "\n").with_context(|| format!("write {}", path.display()))
}

/// Reads a JSON result file; `Ok(None)` if it does not exist.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> anyhow::Result<Option<T>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(
            serde_json::from_str(&s).with_context(|| format!("parse {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}
