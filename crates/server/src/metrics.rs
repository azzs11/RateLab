//! Metrics through the `metrics` crate, exported in Prometheus text format.
//!
//! - `drl_decisions_total{policy, outcome, source}`: counter, `outcome` is
//!   `allowed` or `blocked`, `source` is a [`DecisionSource`] name.
//! - `drl_decision_seconds{source}`: histogram of time spent deciding.
//!
//! [`Stats`] keeps plain totals for the dashboard's `/api/metrics`.

use crate::backend::DecisionSource;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Histogram buckets for decision time, in seconds (1 µs to 100 ms).
const DECISION_BUCKETS: &[f64] = &[
    0.000_001, 0.000_005, 0.000_01, 0.000_05, 0.000_1, 0.000_5, 0.001, 0.005, 0.01, 0.05, 0.1,
];

/// Installs the global Prometheus recorder and returns the handle `/metrics` renders.
pub fn install() -> anyhow::Result<PrometheusHandle> {
    Ok(PrometheusBuilder::new()
        .set_buckets(DECISION_BUCKETS)?
        .install_recorder()?)
}

/// Records one decision.
pub fn record(policy: &str, allowed: bool, source: DecisionSource, elapsed: Duration) {
    let outcome = if allowed { "allowed" } else { "blocked" };
    metrics::counter!(
        "drl_decisions_total",
        "policy" => policy.to_owned(),
        "outcome" => outcome,
        "source" => source.name()
    )
    .increment(1);
    metrics::histogram!("drl_decision_seconds", "source" => source.name())
        .record(elapsed.as_secs_f64());
}

/// Running totals for the dashboard.
#[derive(Debug, Default)]
pub struct Stats {
    allowed: AtomicU64,
    blocked: AtomicU64,
}

impl Stats {
    /// Counts one decision.
    pub fn count(&self, allowed: bool) {
        let counter = if allowed {
            &self.allowed
        } else {
            &self.blocked
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// `(allowed, blocked)` so far.
    pub fn totals(&self) -> (u64, u64) {
        (
            self.allowed.load(Ordering::Relaxed),
            self.blocked.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_count_both_outcomes() {
        let s = Stats::default();
        s.count(true);
        s.count(true);
        s.count(false);
        assert_eq!(s.totals(), (2, 1));
    }

    #[test]
    fn recording_without_a_recorder_is_a_no_op() {
        record("p", true, DecisionSource::Local, Duration::from_micros(3));
    }
}
