//! The exact oracle every measurement is compared against.
//!
//! The rule: **a key never gets more than `limit` units in any time span of length
//! `window`**. The oracle sees each request in order and admits it exactly when doing
//! so keeps that rule. It is written as plainly as possible (a log of admitted
//! requests per key, scanned directly) and shares no code with `ratelab`, so it also
//! checks the library's own sliding window.

use sim::{Arrival, Trace};
use std::collections::{HashMap, VecDeque};

/// Greedy exact limiter: admit a request iff the key's last-`window` total plus its
/// cost stays within `limit`.
#[derive(Debug)]
pub struct ExactOracle {
    limit: u64,
    window_ns: u64,
    logs: HashMap<u32, VecDeque<(u64, u64)>>,
}

impl ExactOracle {
    /// An oracle for `limit` units per `window_ns` nanoseconds.
    pub fn new(limit: u64, window_ns: u64) -> Self {
        Self {
            limit,
            window_ns,
            logs: HashMap::new(),
        }
    }

    /// Decides one request. Arrivals must come in time order.
    pub fn admit(&mut self, a: &Arrival) -> bool {
        let log = self.logs.entry(a.key).or_default();
        // Keep only requests inside (t - window, t].
        while log
            .front()
            .is_some_and(|&(at, _)| a.at.0 - at >= self.window_ns)
        {
            log.pop_front();
        }
        let used: u64 = log.iter().map(|&(_, cost)| cost).sum();
        if used + a.cost.0 <= self.limit {
            log.push_back((a.at.0, a.cost.0));
            true
        } else {
            false
        }
    }

    /// Decides every arrival.
    pub fn decide_all(limit: u64, window_ns: u64, arrivals: &[Arrival]) -> Vec<bool> {
        let mut oracle = Self::new(limit, window_ns);
        arrivals.iter().map(|a| oracle.admit(a)).collect()
    }
}

/// How one run compares with the oracle on the same input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Accuracy {
    /// Requests in the run.
    pub requests: u64,
    /// Requests the limiter allowed.
    pub admitted: u64,
    /// Requests the oracle allowed.
    pub oracle_admitted: u64,
    /// Allowed by the limiter, denied by the oracle (% of requests).
    pub over_pct: f64,
    /// Denied by the limiter, allowed by the oracle (% of requests).
    pub under_pct: f64,
    /// (admitted − oracle admitted) / oracle admitted, in %.
    pub admitted_vs_oracle_pct: f64,
    /// The most units any key got in one window, divided by the limit. The oracle's
    /// value is at most 1 by construction.
    pub worst_window_ratio: f64,
}

/// Compares a limiter's trace with the oracle's decisions on the same arrivals.
pub fn compare(trace: &Trace, oracle: &[bool], limit: u64, window_ns: u64) -> Accuracy {
    let requests = trace.steps.len() as u64;
    let mut admitted = 0;
    let mut over = 0;
    let mut under = 0;
    for (step, &ok) in trace.steps.iter().zip(oracle) {
        let allowed = step.decision.allowed;
        admitted += u64::from(allowed);
        over += u64::from(allowed && !ok);
        under += u64::from(!allowed && ok);
    }
    let oracle_admitted = oracle.iter().filter(|&&ok| ok).count() as u64;
    let pct = |n: u64, d: u64| {
        if d == 0 {
            0.0
        } else {
            n as f64 * 100.0 / d as f64
        }
    };
    let worst = trace.max_admitted_in_window(std::time::Duration::from_nanos(window_ns));
    Accuracy {
        requests,
        admitted,
        oracle_admitted,
        over_pct: pct(over, requests),
        under_pct: pct(under, requests),
        admitted_vs_oracle_pct: if oracle_admitted == 0 {
            0.0
        } else {
            (admitted as f64 - oracle_admitted as f64) * 100.0 / oracle_admitted as f64
        },
        worst_window_ratio: worst as f64 / limit as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratelab::{Algorithm, Cost, ManualClock, Nanos, RateLimitConfig};
    use std::sync::Arc;
    use std::time::Duration;

    fn at(ns: u64, key: u32, cost: u64) -> Arrival {
        Arrival {
            at: Nanos(ns),
            key,
            cost: Cost(cost),
        }
    }

    #[test]
    fn admits_up_to_the_limit_in_any_window() {
        let a = [
            at(0, 0, 1),
            at(1, 0, 1),
            at(2, 0, 1),
            at(9, 0, 1),
            at(10, 0, 1),
        ];
        // 2 per 10 ns: the third is denied; at t=10 the first has left the window.
        assert_eq!(
            ExactOracle::decide_all(2, 10, &a),
            [true, true, false, false, true]
        );
    }

    #[test]
    fn keys_and_costs_are_independent() {
        let a = [at(0, 0, 3), at(0, 1, 3), at(1, 0, 1), at(1, 1, 2)];
        assert_eq!(
            ExactOracle::decide_all(4, 100, &a),
            [true, true, true, false]
        );
    }

    #[test]
    fn library_sliding_window_matches_the_oracle_exactly() {
        let config = RateLimitConfig::new(7, Duration::from_millis(30)).unwrap();
        let arrivals = sim::poisson(3, Duration::from_secs(5), Duration::from_micros(700), 3);
        let clock = Arc::new(ManualClock::new());
        let sliding = Algorithm::SlidingWindow.build(config, Arc::clone(&clock));
        let trace = sim::run(&sliding, &clock, &arrivals);
        let oracle = ExactOracle::decide_all(7, config.window_nanos(), &arrivals);
        let acc = compare(&trace, &oracle, 7, config.window_nanos());
        assert_eq!(acc.over_pct, 0.0);
        assert_eq!(acc.under_pct, 0.0);
        assert!(acc.worst_window_ratio <= 1.0);
    }

    #[test]
    fn fixed_window_over_admits_on_the_boundary_attack() {
        let config = RateLimitConfig::per_second(10).unwrap();
        let arrivals = sim::boundary_attack(config, Duration::from_millis(1));
        let clock = Arc::new(ManualClock::new());
        let fixed = Algorithm::FixedWindow.build(config, Arc::clone(&clock));
        let trace = sim::run(&fixed, &clock, &arrivals);
        let oracle = ExactOracle::decide_all(10, config.window_nanos(), &arrivals);
        let acc = compare(&trace, &oracle, 10, config.window_nanos());
        assert_eq!(acc.admitted, 20);
        assert_eq!(acc.oracle_admitted, 10);
        assert_eq!(acc.over_pct, 50.0);
        assert_eq!(acc.worst_window_ratio, 2.0);
    }
}
