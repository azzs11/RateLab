//! The algorithm lab: one limiter, every decision, and its state over time.
//!
//! State between requests is read with zero-cost checks, which every algorithm answers
//! without charging anything (checked by the conformance suite). `remaining` is the
//! bucket level or what is left of the window; for GCRA, `now + reset_after` is the
//! theoretical arrival time.

use crate::traffic::{Traffic, config, ms_to_nanos};
use ratelab::{Algorithm, Cost, ManualClock, Nanos};
use sim::{Step, Trace};
use std::sync::Arc;
use wasm_bindgen::prelude::*;

/// A limiter run over some traffic.
#[wasm_bindgen]
#[derive(Debug, Clone, PartialEq)]
pub struct LabRun {
    allowed: Vec<u8>,
    remaining: Vec<u32>,
    reset_ms: Vec<f64>,
    retry_ms: Vec<f64>,
    sample_at_ms: Vec<f64>,
    sample_remaining: Vec<u32>,
    sample_reset_ms: Vec<f64>,
    admitted: u32,
    exact_admitted: u32,
    worst_window: u32,
}

fn ms(d: std::time::Duration) -> f64 {
    d.as_nanos() as f64 / 1e6
}

impl LabRun {
    /// Runs `algorithm` over `traffic`, sampling key `watch` every `sample_ms`.
    pub fn build(
        algorithm: &str,
        limit: u32,
        window_ms: f64,
        traffic: &Traffic,
        watch: u32,
        sample_ms: f64,
    ) -> Result<LabRun, String> {
        let algorithm: Algorithm = algorithm.parse().map_err(|e| format!("{e}"))?;
        let config = config(limit, window_ms)?;
        let step = ms_to_nanos(sample_ms).max(100_000); // at most one sample per 0.1 ms
        let end = traffic.arrivals.last().map_or(0, |a| a.at.0) + config.window_nanos();
        if end / step > 200_000 {
            return Err("too many samples: raise sample_ms".to_owned());
        }

        let clock = Arc::new(ManualClock::new());
        let limiter = algorithm.build(config, Arc::clone(&clock));
        let watch_key = format!("k{watch}");
        let mut run = LabRun {
            allowed: Vec::with_capacity(traffic.len()),
            remaining: Vec::with_capacity(traffic.len()),
            reset_ms: Vec::with_capacity(traffic.len()),
            retry_ms: Vec::with_capacity(traffic.len()),
            sample_at_ms: Vec::new(),
            sample_remaining: Vec::new(),
            sample_reset_ms: Vec::new(),
            admitted: 0,
            exact_admitted: 0,
            worst_window: 0,
        };
        let mut next_sample = 0u64;
        let mut sample_until = |t: u64, run: &mut LabRun| {
            while next_sample <= t {
                clock.set(Nanos(next_sample));
                let d = limiter.check(&watch_key, Cost(0));
                run.sample_at_ms.push(next_sample as f64 / 1e6);
                run.sample_remaining
                    .push(u32::try_from(d.remaining.0).unwrap_or(u32::MAX));
                run.sample_reset_ms.push(ms(d.reset_after));
                next_sample += step;
            }
        };
        let mut steps = Vec::with_capacity(traffic.len());
        for a in &traffic.arrivals {
            sample_until(a.at.0.saturating_sub(1), &mut run);
            clock.set(a.at);
            let d = limiter.check(&format!("k{}", a.key), a.cost);
            run.allowed.push(u8::from(d.allowed));
            run.remaining
                .push(u32::try_from(d.remaining.0).unwrap_or(u32::MAX));
            run.reset_ms.push(ms(d.reset_after));
            run.retry_ms.push(d.retry_after.map_or(f64::NAN, ms));
            run.admitted += u32::from(d.allowed);
            steps.push(Step {
                arrival: *a,
                decision: d,
            });
        }
        sample_until(end, &mut run);
        run.worst_window = u32::try_from(Trace { steps }.max_admitted_in_window(config.window()))
            .unwrap_or(u32::MAX);

        // The exact reference: the sliding window log admits exactly what the rule allows.
        let clock = Arc::new(ManualClock::new());
        let exact = Algorithm::SlidingWindow.build(config, Arc::clone(&clock));
        run.exact_admitted =
            u32::try_from(sim::run(&exact, &clock, &traffic.arrivals).admitted_units())
                .unwrap_or(u32::MAX);
        Ok(run)
    }
}

#[wasm_bindgen]
impl LabRun {
    /// Runs `algorithm` over `traffic`, sampling the state of key `watch` every
    /// `sample_ms`. Throws on bad input.
    pub fn run(
        algorithm: &str,
        limit: u32,
        window_ms: f64,
        traffic: &Traffic,
        watch: u32,
        sample_ms: f64,
    ) -> Result<LabRun, JsError> {
        LabRun::build(algorithm, limit, window_ms, traffic, watch, sample_ms)
            .map_err(|e| JsError::new(&e))
    }

    /// 1 if request `i` was allowed, else 0.
    pub fn allowed(&self) -> Vec<u8> {
        self.allowed.clone()
    }

    /// Remaining units for the request's key after each decision.
    pub fn remaining(&self) -> Vec<u32> {
        self.remaining.clone()
    }

    /// `reset_after` of each decision, ms.
    #[wasm_bindgen(js_name = resetMs)]
    pub fn reset_ms(&self) -> Vec<f64> {
        self.reset_ms.clone()
    }

    /// `retry_after` of each decision, ms (NaN when allowed or never possible).
    #[wasm_bindgen(js_name = retryMs)]
    pub fn retry_ms(&self) -> Vec<f64> {
        self.retry_ms.clone()
    }

    /// Times of the state samples, ms.
    #[wasm_bindgen(js_name = sampleAtMs)]
    pub fn sample_at_ms(&self) -> Vec<f64> {
        self.sample_at_ms.clone()
    }

    /// Remaining units of the watched key at each sample.
    #[wasm_bindgen(js_name = sampleRemaining)]
    pub fn sample_remaining(&self) -> Vec<u32> {
        self.sample_remaining.clone()
    }

    /// `reset_after` of the watched key at each sample, ms (for GCRA: TAT − now).
    #[wasm_bindgen(js_name = sampleResetMs)]
    pub fn sample_reset_ms(&self) -> Vec<f64> {
        self.sample_reset_ms.clone()
    }

    /// Requests allowed.
    pub fn admitted(&self) -> u32 {
        self.admitted
    }

    /// Requests the exact rule (sliding window log) allows on the same traffic.
    #[wasm_bindgen(js_name = exactAdmitted)]
    pub fn exact_admitted(&self) -> u32 {
        self.exact_admitted
    }

    /// The most units any key got in one window.
    #[wasm_bindgen(js_name = worstWindow)]
    pub fn worst_window(&self) -> u32 {
        self.worst_window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_attack_lets_fixed_window_double_the_limit() {
        let t = Traffic::build("boundary_attack", 0, 2.5, 10, 1_000.0, 1, 1.0).unwrap();
        let fixed = LabRun::build("fixed_window", 10, 1_000.0, &t, 0, 10.0).unwrap();
        let gcra = LabRun::build("gcra", 10, 1_000.0, &t, 0, 10.0).unwrap();
        assert_eq!(fixed.worst_window(), 20);
        assert_eq!(gcra.worst_window(), 10);
        assert_eq!(fixed.exact_admitted(), 10);
        assert_eq!(fixed.admitted(), 20);
    }

    #[test]
    fn samples_cover_the_run_and_peeking_charges_nothing() {
        let t = Traffic::build("steady", 4, 2.0, 5, 1_000.0, 1, 2.0).unwrap();
        let with_samples = LabRun::build("token_bucket", 5, 1_000.0, &t, 0, 1.0).unwrap();
        let sparse = LabRun::build("token_bucket", 5, 1_000.0, &t, 0, 500.0).unwrap();
        assert_eq!(with_samples.allowed(), sparse.allowed());
        assert!(with_samples.sample_at_ms().len() > 2_000);
        assert!(with_samples.sample_remaining().iter().all(|&r| r <= 5));
    }

    #[test]
    fn retry_is_nan_only_when_allowed() {
        let t = Traffic::build("steady", 5, 2.0, 3, 1_000.0, 1, 4.0).unwrap();
        let run = LabRun::build("gcra", 3, 1_000.0, &t, 0, 50.0).unwrap();
        for (a, r) in run.allowed().iter().zip(run.retry_ms()) {
            assert_eq!(*a == 1, r.is_nan());
        }
    }
}
