//! Summaries across repeated runs, percentiles and Jain's fairness index.

use serde::{Deserialize, Serialize};

/// Mean and spread of one metric over several runs (seeds or repetitions).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Number of runs.
    pub n: usize,
    /// Arithmetic mean.
    pub mean: f64,
    /// Smallest value.
    pub min: f64,
    /// Largest value.
    pub max: f64,
    /// Sample standard deviation (0 for a single run).
    pub stddev: f64,
}

impl Summary {
    /// Summarises `values`. An empty slice gives all zeros.
    pub fn of(values: &[f64]) -> Self {
        let n = values.len();
        if n == 0 {
            return Self {
                n,
                mean: 0.0,
                min: 0.0,
                max: 0.0,
                stddev: 0.0,
            };
        }
        let mean = values.iter().sum::<f64>() / n as f64;
        let var = if n > 1 {
            values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1) as f64
        } else {
            0.0
        };
        Self {
            n,
            mean,
            min: values.iter().copied().fold(f64::INFINITY, f64::min),
            max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            stddev: var.sqrt(),
        }
    }
}

/// Nearest-rank percentile of sorted `values` (`p` in 0..=100). 0 if empty.
pub fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Jain's fairness index: `(Σx)² / (n · Σx²)`. 1 means perfectly equal; `1/n` means
/// one party got everything. 1 for an empty or all-zero input.
pub fn jain(values: &[f64]) -> f64 {
    let sum: f64 = values.iter().sum();
    let squares: f64 = values.iter().map(|v| v * v).sum();
    if values.is_empty() || squares == 0.0 {
        1.0
    } else {
        sum * sum / (values.len() as f64 * squares)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_of_known_values() {
        let s = Summary::of(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);
        assert_eq!(s.mean, 5.0);
        assert_eq!(s.min, 2.0);
        assert_eq!(s.max, 9.0);
        assert!((s.stddev - 2.138).abs() < 1e-3);
        assert_eq!(Summary::of(&[3.0]).stddev, 0.0);
        assert_eq!(Summary::of(&[]).n, 0);
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&v, 50.0), 50);
        assert_eq!(percentile(&v, 99.0), 99);
        assert_eq!(percentile(&v, 99.9), 100);
        assert_eq!(percentile(&v, 0.0), 1);
        assert_eq!(percentile(&[], 50.0), 0);
    }

    #[test]
    fn jain_bounds() {
        assert_eq!(jain(&[5.0, 5.0, 5.0]), 1.0);
        assert!((jain(&[1.0, 0.0, 0.0, 0.0]) - 0.25).abs() < 1e-12);
        assert_eq!(jain(&[]), 1.0);
    }
}
