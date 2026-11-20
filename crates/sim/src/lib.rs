//! Deterministic traffic simulation for `ratelab` limiters.
//!
//! Generate a list of [`Arrival`]s from a seed, replay them through any
//! [`RateLimiter`] on a [`ManualClock`], and measure the resulting [`Trace`]. The same
//! seed always gives the same arrivals and the same decisions, on every platform:
//! everything here is integer arithmetic.
//!
//! [`run`] replays traffic through one limiter; [`cluster::run_cluster`] simulates N
//! nodes coordinating through a shared store, with network delay, clock skew and
//! outages.
//!
//! ```
//! use ratelab::{Algorithm, ManualClock, RateLimitConfig};
//! use sim::{boundary_attack, run};
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! let config = RateLimitConfig::per_second(10)?;
//! let arrivals = boundary_attack(config, Duration::from_millis(1));
//!
//! let clock = Arc::new(ManualClock::new());
//! let fixed = Algorithm::FixedWindow.build(config, Arc::clone(&clock));
//! let trace = run(&fixed, &clock, &arrivals);
//! assert_eq!(trace.max_admitted_in_window(config.window()), 20); // 2 × limit slips through
//!
//! let clock = Arc::new(ManualClock::new());
//! let sliding = Algorithm::SlidingWindow.build(config, Arc::clone(&clock));
//! let trace = run(&sliding, &clock, &arrivals);
//! assert_eq!(trace.max_admitted_in_window(config.window()), 10);
//! # Ok::<(), ratelab::ConfigError>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use ratelab::{Clock, Cost, Decision, ManualClock, Nanos, RateLimitConfig, RateLimiter};
use std::time::Duration;

pub mod cluster;
pub mod metrics;
pub mod overload;
mod scenarios;
pub mod tokens;
pub use scenarios::{diurnal, flash_crowd, merge, on_off, pareto, poisson};

/// A small, fast, seeded PRNG (SplitMix64). Integer-only, so identical everywhere.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// A generator seeded with `seed`.
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (0 if `n` is 0). Uses multiply-shift, so there is no modulo bias.
    ///
    /// ```
    /// let mut rng = sim::SplitMix64::new(1);
    /// assert!((0..1000).all(|_| rng.below(10) < 10));
    /// ```
    pub fn below(&mut self, n: u64) -> u64 {
        ((u128::from(self.next_u64()) * u128::from(n)) >> 64) as u64
    }
}

/// One request in a scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arrival {
    /// When the request arrives (scenario time starts at [`Nanos::ZERO`]).
    pub at: Nanos,
    /// Which client sends it; turned into the key `"k{key}"`.
    pub key: u32,
    /// Quota units it asks for.
    pub cost: Cost,
}

/// Evenly paced traffic with random jitter: `count` requests from `keys` clients, one
/// every `mean_gap` on average (each gap is uniform in `0..2 × mean_gap`).
///
/// ```
/// use std::time::Duration;
///
/// let a = sim::jittered(1, 100, Duration::from_millis(10), 3);
/// assert_eq!(a.len(), 100);
/// assert_eq!(a, sim::jittered(1, 100, Duration::from_millis(10), 3));
/// ```
pub fn jittered(seed: u64, count: usize, mean_gap: Duration, keys: u32) -> Vec<Arrival> {
    let mut rng = SplitMix64::new(seed);
    let span = 2 * Nanos::from_duration(mean_gap).0;
    let mut now = 0u64;
    (0..count)
        .map(|_| {
            now = now.saturating_add(rng.below(span));
            Arrival {
                at: Nanos(now),
                key: rng.below(u64::from(keys.max(1))) as u32,
                cost: Cost::ONE,
            }
        })
        .collect()
}

/// On/off traffic from one client: `bursts` bursts of `burst_size` back-to-back
/// requests (1 ns apart), separated by `pause`.
///
/// ```
/// use std::time::Duration;
///
/// let a = sim::bursty(3, 4, Duration::from_millis(500));
/// assert_eq!(a.len(), 12);
/// ```
pub fn bursty(bursts: usize, burst_size: usize, pause: Duration) -> Vec<Arrival> {
    let pause = Nanos::from_duration(pause).0;
    let mut out = Vec::with_capacity(bursts * burst_size);
    let mut now = 0u64;
    for _ in 0..bursts {
        for _ in 0..burst_size {
            out.push(Arrival {
                at: Nanos(now),
                key: 0,
                cost: Cost::ONE,
            });
            now = now.saturating_add(1);
        }
        now = now.saturating_add(pause);
    }
    out
}

/// The classic fixed-window boundary attack: one client sends `limit` requests just
/// before the end of the first window, then `limit` more just after it, `gap` later.
pub fn boundary_attack(config: RateLimitConfig, gap: Duration) -> Vec<Arrival> {
    let window = config.window_nanos();
    let gap = Nanos::from_duration(gap).0;
    let first = window.saturating_sub(gap / 2).saturating_sub(1);
    let second = first.saturating_add(gap);
    let wave = |at: u64| {
        (0..config.limit()).map(move |_| Arrival {
            at: Nanos(at),
            key: 0,
            cost: Cost::ONE,
        })
    };
    wave(first).chain(wave(second)).collect()
}

/// One decision in a [`Trace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// The request.
    pub arrival: Arrival,
    /// What the limiter decided.
    pub decision: Decision,
}

/// Everything that happened in one run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trace {
    /// One entry per arrival, in arrival order.
    pub steps: Vec<Step>,
}

impl Trace {
    /// Total units admitted.
    pub fn admitted_units(&self) -> u64 {
        self.steps
            .iter()
            .filter(|s| s.decision.allowed)
            .map(|s| s.arrival.cost.0)
            .sum()
    }

    /// Number of denied requests.
    pub fn denied(&self) -> usize {
        self.steps.iter().filter(|s| !s.decision.allowed).count()
    }

    /// The most units admitted for a single key in any span `[t, t + window)`:
    /// the worst-case burst a client actually got through.
    pub fn max_admitted_in_window(&self, window: Duration) -> u64 {
        let w = Nanos::from_duration(window).0;
        let mut per_key: std::collections::BTreeMap<u32, Vec<(u64, u64)>> = Default::default();
        for s in self.steps.iter().filter(|s| s.decision.allowed) {
            per_key
                .entry(s.arrival.key)
                .or_default()
                .push((s.arrival.at.0, s.arrival.cost.0));
        }
        let mut worst = 0;
        for admitted in per_key.values() {
            let (mut lo, mut sum) = (0usize, 0u64);
            for &(at, cost) in admitted {
                sum += cost;
                while at - admitted[lo].0 >= w {
                    sum -= admitted[lo].1;
                    lo += 1;
                }
                worst = worst.max(sum);
            }
        }
        worst
    }
}

/// Replays `arrivals` (sorted by time) through `limiter`, moving `clock` to each
/// arrival time first. The clock never moves backwards.
pub fn run<L: RateLimiter + ?Sized>(
    limiter: &L,
    clock: &ManualClock,
    arrivals: &[Arrival],
) -> Trace {
    let start = clock.now();
    let steps = arrivals
        .iter()
        .map(|&arrival| {
            clock.set(Nanos(start.0.saturating_add(arrival.at.0)));
            let decision = limiter.check(&format!("k{}", arrival.key), arrival.cost);
            Step { arrival, decision }
        })
        .collect();
    Trace { steps }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratelab::Algorithm;
    use std::sync::Arc;

    fn replay(a: Algorithm, config: RateLimitConfig, arrivals: &[Arrival]) -> Trace {
        let clock = Arc::new(ManualClock::new());
        let limiter = a.build(config, Arc::clone(&clock));
        run(&limiter, &clock, arrivals)
    }

    #[test]
    fn rng_is_reproducible_and_seed_sensitive() {
        let draw = |seed| {
            let mut r = SplitMix64::new(seed);
            (0..5).map(|_| r.next_u64()).collect::<Vec<_>>()
        };
        assert_eq!(draw(42), draw(42));
        assert_ne!(draw(42), draw(43));
    }

    #[test]
    fn below_stays_in_range() {
        let mut r = SplitMix64::new(7);
        assert!((0..10_000).all(|_| r.below(3) < 3));
        assert_eq!(r.below(0), 0);
    }

    #[test]
    fn same_seed_same_trace() {
        let config = RateLimitConfig::per_second(20).unwrap();
        let arrivals = jittered(9, 2_000, Duration::from_millis(20), 4);
        for a in Algorithm::ALL {
            assert_eq!(
                replay(a, config, &arrivals),
                replay(a, config, &arrivals),
                "{a}"
            );
        }
    }

    #[test]
    fn jittered_arrivals_are_sorted() {
        let arrivals = jittered(1, 1_000, Duration::from_micros(50), 2);
        assert!(arrivals.windows(2).all(|w| w[0].at <= w[1].at));
    }

    #[test]
    fn boundary_attack_separates_the_algorithms() {
        let config = RateLimitConfig::per_second(50).unwrap();
        let arrivals = boundary_attack(config, Duration::from_millis(2));
        let window = config.window();
        let worst = |a| replay(a, config, &arrivals).max_admitted_in_window(window);
        assert_eq!(worst(Algorithm::FixedWindow), 100);
        assert_eq!(worst(Algorithm::SlidingWindow), 50);
        // The buckets refill only during the 2 ms gap: 0.1 of a unit at 50 per second,
        // so nothing extra gets through.
        assert_eq!(worst(Algorithm::TokenBucket), 50);
        assert_eq!(worst(Algorithm::LeakyBucket), 50);
        assert_eq!(worst(Algorithm::Gcra), 50);
    }

    #[test]
    fn bursty_traffic_is_capped_per_burst() {
        let config = RateLimitConfig::per_second(5).unwrap();
        let arrivals = bursty(4, 10, Duration::from_secs(2));
        for a in Algorithm::ALL {
            let trace = replay(a, config, &arrivals);
            // Each burst gets a fresh full quota after the 2 s pause.
            assert_eq!(trace.admitted_units(), 20, "{a}");
            assert_eq!(trace.denied(), 20, "{a}");
        }
    }
}
