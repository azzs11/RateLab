//! How a cluster run compares with one perfectly coordinated limiter.
//!
//! The **ideal** is a single exact token bucket fed the same arrival times. **Admission
//! error** counts units per key and per window: units admitted above the ideal are
//! over-admission, units missing below it are under-admission, both as % of what the
//! ideal admitted. Which individual requests got through under overload does not
//! matter, only how many; the share of individual decisions that differ is reported
//! separately as `disagree_pct`.

use crate::cluster::{ClusterArrival, ClusterRun};
use crate::{Step, Trace};
use drl_core::{Algorithm, ManualClock, RateLimitConfig};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

/// Nearest-rank percentile of sorted `values` (`p` in 0..=100); 0 if empty.
pub fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Decisions of the ideal (one exact token bucket) on the same arrival times.
///
/// ```
/// use drl_core::{Cost, Nanos, RateLimitConfig};
/// use sim::cluster::ClusterArrival;
/// use sim::Arrival;
///
/// let config = RateLimitConfig::per_second(1)?;
/// let a: Vec<_> = (0..2)
///     .map(|i| ClusterArrival { arrival: Arrival { at: Nanos(i), key: 0, cost: Cost::ONE }, node: 0 })
///     .collect();
/// assert_eq!(sim::metrics::ideal(config, &a), [true, false]);
/// # Ok::<(), drl_core::ConfigError>(())
/// ```
pub fn ideal(config: RateLimitConfig, a: &[ClusterArrival]) -> Vec<bool> {
    let clock = Arc::new(ManualClock::new());
    let bucket = Algorithm::TokenBucket.build(config, Arc::clone(&clock));
    let arrivals: Vec<_> = a.iter().map(|c| c.arrival).collect();
    crate::run(&bucket, &clock, &arrivals)
        .steps
        .iter()
        .map(|s| s.decision.allowed)
        .collect()
}

/// Over- and under-admitted units against the ideal, counted per key and per bin of
/// `bin_ns`, for arrivals in `range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowCounts {
    /// Units admitted above the ideal, summed over bins.
    pub over: u64,
    /// Units missing below the ideal, summed over bins.
    pub under: u64,
    /// Units the ideal admitted.
    pub ideal: u64,
}

impl WindowCounts {
    /// Over-admission as % of the ideal's admitted units.
    pub fn over_pct(&self) -> f64 {
        self.over as f64 * 100.0 / self.ideal.max(1) as f64
    }

    /// Under-admission as % of the ideal's admitted units.
    pub fn under_pct(&self) -> f64 {
        self.under as f64 * 100.0 / self.ideal.max(1) as f64
    }
}

/// Counts admission error per key and per bin.
pub fn window_counts(
    run: &ClusterRun,
    ideal: &[bool],
    range: Range<u64>,
    bin_ns: u64,
) -> WindowCounts {
    let mut bins: HashMap<(u32, u64), (u64, u64)> = HashMap::new();
    for (o, &ok) in run.outcomes.iter().zip(ideal) {
        let t = o.arrival.at.0;
        if !range.contains(&t) {
            continue;
        }
        let cost = o.arrival.cost.0;
        let entry = bins
            .entry((o.arrival.key, (t - range.start) / bin_ns.max(1)))
            .or_default();
        if o.decision.allowed {
            entry.0 += cost;
        }
        if ok {
            entry.1 += cost;
        }
    }
    bins.values()
        .fold(WindowCounts::default(), |mut acc, &(got, want)| {
            acc.over += got.saturating_sub(want);
            acc.under += want.saturating_sub(got);
            acc.ideal += want;
            acc
        })
}

/// One run measured against the ideal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    /// Requests.
    pub requests: f64,
    /// Over-admission, % of the ideal's admitted units.
    pub over_pct: f64,
    /// Under-admission, % of the ideal's admitted units.
    pub under_pct: f64,
    /// Individual decisions that differ from the ideal, % of requests.
    pub disagree_pct: f64,
    /// Admitted vs ideal admitted, %.
    pub admitted_vs_ideal_pct: f64,
    /// Most units one key got in one window / limit.
    pub worst_window_ratio: f64,
    /// Messages to the store per 1,000 decisions.
    pub calls_per_1k: f64,
    /// Decision latency percentiles, µs.
    pub p50_us: f64,
    /// 99th percentile, µs.
    pub p99_us: f64,
    /// 99.9th percentile, µs.
    pub p999_us: f64,
}

/// Compares `run` with `ideal` decisions on the same arrivals, for a cluster enforcing
/// `config`. Admission error is counted per key and per window of `config`.
pub fn measure(run: &ClusterRun, ideal: &[bool], config: RateLimitConfig) -> Measured {
    let n = run.outcomes.len().max(1) as f64;
    let mut disagree = 0usize;
    let mut admitted = 0usize;
    for (o, &ok) in run.outcomes.iter().zip(ideal) {
        admitted += usize::from(o.decision.allowed);
        disagree += usize::from(o.decision.allowed != ok);
    }
    let counts = window_counts(run, ideal, 0..u64::MAX, config.window_nanos());
    let ideal_admitted = ideal.iter().filter(|&&x| x).count().max(1) as f64;
    let trace = Trace {
        steps: run
            .outcomes
            .iter()
            .map(|o| Step {
                arrival: o.arrival,
                decision: o.decision,
            })
            .collect(),
    };
    let mut latency: Vec<u64> = run
        .outcomes
        .iter()
        .map(|o| o.latency().as_nanos() as u64)
        .collect();
    latency.sort_unstable();
    let us = |p| percentile(&latency, p) as f64 / 1_000.0;
    Measured {
        requests: run.outcomes.len() as f64,
        over_pct: counts.over_pct(),
        under_pct: counts.under_pct(),
        disagree_pct: disagree as f64 * 100.0 / n,
        admitted_vs_ideal_pct: (admitted as f64 - ideal_admitted) * 100.0 / ideal_admitted,
        worst_window_ratio: trace.max_admitted_in_window(config.window()) as f64
            / config.limit() as f64,
        calls_per_1k: run.messages_sent as f64 * 1_000.0 / n,
        p50_us: us(50.0),
        p99_us: us(99.0),
        p999_us: us(99.9),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drl_core::Nanos;
    use std::time::Duration;

    #[test]
    fn window_counts_ignore_which_requests_got_through() {
        let ms = 1_000_000;
        let outcome = |at: u64, allowed: bool| crate::cluster::Outcome {
            arrival: crate::Arrival {
                at: Nanos(at),
                key: 0,
                cost: drl_core::Cost::ONE,
            },
            node: 0,
            decided_at: Nanos(at),
            decision: if allowed {
                drl_core::Decision::allow(drl_core::Tokens(0), Duration::ZERO)
            } else {
                drl_core::Decision::deny(drl_core::Tokens(0), Duration::ZERO, None)
            },
        };
        let run = |allowed: [bool; 4]| ClusterRun {
            outcomes: (0..4)
                .map(|i| outcome(i * 100 * ms, allowed[i as usize]))
                .collect(),
            undecided: 0,
            messages_sent: 0,
            store_ops: 0,
            timeouts: 0,
            messages: Vec::new(),
            samples: Vec::new(),
        };
        let ideal = [true, true, false, false];
        // Same count, different requests: no admission error.
        let swapped = window_counts(
            &run([false, false, true, true]),
            &ideal,
            0..u64::MAX,
            1_000 * ms,
        );
        assert_eq!((swapped.over, swapped.under, swapped.ideal), (0, 0, 2));
        // One extra admitted.
        let extra = window_counts(
            &run([true, true, true, false]),
            &ideal,
            0..u64::MAX,
            1_000 * ms,
        );
        assert_eq!(extra.over_pct(), 50.0);
        assert_eq!(extra.under_pct(), 0.0);
    }
}
