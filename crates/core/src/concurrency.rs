//! Concurrency limits: how many requests may be *in progress* at once.
//!
//! A rate limit caps requests per window. It cannot see that a backend got slower: it
//! keeps admitting the same rate, queues grow, and every request waits until it times
//! out (the backend still does the work, so goodput collapses). A concurrency limit
//! caps requests in flight instead, and an adaptive one lowers itself when latency or
//! timeouts say the backend is saturated, shedding the excess with a fast rejection.
//!
//! [`ConcurrencyLimiter`] counts requests in flight. Its [`LimitAlgorithm`] picks the
//! limit from what each finished request reports ([`Sample`]): how long it took,
//! how many were in flight when it started, and whether it was dropped (timed out or
//! failed because of overload).
//!
//! - [`Concurrency::Fixed`]: a static limit.
//! - [`Concurrency::Aimd`]: additive increase, multiplicative decrease, as in TCP.
//! - [`Concurrency::Gradient2`]: compares recent latency with a long-term average.
//!
//! Both adaptive algorithms follow the ones in Netflix's `concurrency-limits` library.
//! Time is injected: callers pass the current time and the measured latency.
//!
//! ```
//! use ratelab::concurrency::{Completion, Concurrency, ConcurrencyLimiter};
//! use ratelab::Nanos;
//! use std::time::Duration;
//!
//! let limiter = ConcurrencyLimiter::new(Concurrency::Fixed(2));
//! let a = limiter.try_acquire().unwrap();
//! let b = limiter.try_acquire().unwrap();
//! assert!(limiter.try_acquire().is_none()); // a third request is shed
//!
//! limiter.release(a, Nanos(0), Duration::from_millis(5), Completion::Success);
//! assert!(limiter.try_acquire().is_some());
//! # limiter.release(b, Nanos(0), Duration::ZERO, Completion::Ignored);
//! ```

use crate::Nanos;
use std::fmt::Debug;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// What one finished request tells the limit algorithm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// When the request finished.
    pub now: Nanos,
    /// How long it took.
    pub rtt: Duration,
    /// Requests in flight when it started, itself included.
    pub inflight: usize,
    /// It timed out or failed because of overload.
    pub dropped: bool,
}

/// Picks a concurrency limit from finished requests.
pub trait LimitAlgorithm: Send + Debug {
    /// The current limit (at least 1).
    fn limit(&self) -> usize;
    /// Learns from one finished request.
    fn on_sample(&mut self, sample: Sample);
}

/// A static limit that never changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedLimit(usize);

impl FixedLimit {
    /// A limit of `limit` (at least 1).
    ///
    /// ```
    /// use ratelab::concurrency::{FixedLimit, LimitAlgorithm};
    /// assert_eq!(FixedLimit::new(0).limit(), 1);
    /// ```
    pub fn new(limit: usize) -> Self {
        Self(limit.max(1))
    }
}

impl LimitAlgorithm for FixedLimit {
    fn limit(&self) -> usize {
        self.0
    }
    fn on_sample(&mut self, _: Sample) {}
}

/// Settings for [`Aimd`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AimdConfig {
    /// Starting limit.
    pub initial: usize,
    /// Lowest limit.
    pub min: usize,
    /// Highest limit.
    pub max: usize,
    /// Factor applied on a drop (0 < backoff < 1).
    pub backoff: f64,
    /// A request slower than this counts as a drop.
    pub latency_threshold: Duration,
}

impl Default for AimdConfig {
    /// Netflix's defaults (initial 20, backoff 0.9, 5 s threshold), with a minimum of 1
    /// and a maximum of 1,000.
    fn default() -> Self {
        Self {
            initial: 20,
            min: 1,
            max: 1_000,
            backoff: 0.9,
            latency_threshold: Duration::from_secs(5),
        }
    }
}

/// Additive increase, multiplicative decrease. Each drop (or request slower than the
/// threshold) multiplies the limit by `backoff`; each other request adds 1, but only
/// while the limit is in use (at least half of it in flight).
///
/// It reacts only to drops, so latency can rise up to the threshold before it backs off.
///
/// ```
/// use ratelab::concurrency::{Aimd, AimdConfig, LimitAlgorithm, Sample};
/// use ratelab::Nanos;
/// use std::time::Duration;
///
/// let mut aimd = Aimd::new(AimdConfig { initial: 10, ..AimdConfig::default() });
/// let ok = Sample { now: Nanos(0), rtt: Duration::from_millis(5), inflight: 10, dropped: false };
/// aimd.on_sample(ok);
/// assert_eq!(aimd.limit(), 11);
/// aimd.on_sample(Sample { dropped: true, ..ok });
/// assert_eq!(aimd.limit(), 9); // 11 × 0.9, rounded down
/// ```
#[derive(Debug, Clone)]
pub struct Aimd {
    config: AimdConfig,
    limit: usize,
}

impl Aimd {
    /// An AIMD limit with `config`.
    ///
    /// ```
    /// use ratelab::concurrency::{Aimd, AimdConfig, LimitAlgorithm};
    /// assert_eq!(Aimd::new(AimdConfig::default()).limit(), 20);
    /// ```
    pub fn new(config: AimdConfig) -> Self {
        let min = config.min.max(1);
        let max = config.max.max(min);
        Self {
            config: AimdConfig { min, max, ..config },
            limit: config.initial.clamp(min, max),
        }
    }
}

impl LimitAlgorithm for Aimd {
    fn limit(&self) -> usize {
        self.limit
    }

    fn on_sample(&mut self, s: Sample) {
        let c = &self.config;
        if s.dropped || s.rtt > c.latency_threshold {
            self.limit = ((self.limit as f64 * c.backoff) as usize).max(c.min);
        } else if s.inflight * 2 >= self.limit {
            self.limit = (self.limit + 1).min(c.max);
        }
    }
}

/// Settings for [`Gradient2`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gradient2Config {
    /// Starting limit.
    pub initial: usize,
    /// Lowest limit.
    pub min: usize,
    /// Highest limit.
    pub max: usize,
    /// Weight of each new estimate (0 < smoothing ≤ 1).
    pub smoothing: f64,
    /// How much slower than the long-term latency is still fine (≥ 1).
    pub tolerance: f64,
    /// Extra requests allowed beyond `limit × gradient`, so the limit can grow.
    pub queue_size: usize,
    /// Updates averaged by the long-term latency (an exponential moving average).
    pub long_window: u32,
    /// Shortest time between updates.
    pub min_window: Duration,
    /// Longest time between updates.
    pub max_window: Duration,
    /// Requests needed before a window counts.
    pub min_samples: u32,
}

impl Default for Gradient2Config {
    /// Netflix's defaults (smoothing 0.2, tolerance 1.5, queue size 4, long window 600,
    /// 10 samples per window) with a minimum of 1, a maximum of 1,000, and windows of
    /// 100 ms to 1 s instead of 1 s, so it reacts within a short benchmark.
    fn default() -> Self {
        Self {
            initial: 20,
            min: 1,
            max: 1_000,
            smoothing: 0.2,
            tolerance: 1.5,
            queue_size: 4,
            long_window: 600,
            min_window: Duration::from_millis(100),
            max_window: Duration::from_secs(1),
            min_samples: 10,
        }
    }
}

/// Latency gradient. Requests are grouped into short windows; at the end of each, the
/// window's average latency (short) is compared with a slow moving average (long):
///
/// ```text
/// gradient = clamp(tolerance × long / short, 0.5, 1)
/// limit    = (1 − smoothing) × limit + smoothing × (limit × gradient + queue_size)
/// ```
///
/// While latency stays within `tolerance` of normal the limit creeps up by
/// `queue_size`; when queues build up and latency rises, it shrinks, by at most half
/// per window. It does not grow while less than half the limit is in use. Unlike
/// Netflix's version, timed-out requests count with their (long) latency, so a window
/// of only timeouts still lowers the limit.
///
/// ```
/// use ratelab::concurrency::{Gradient2, Gradient2Config, LimitAlgorithm, Sample};
/// use ratelab::Nanos;
/// use std::time::Duration;
///
/// let mut g = Gradient2::new(Gradient2Config::default());
/// let at = |ms: u64, rtt_ms: u64| Sample {
///     now: Nanos(ms * 1_000_000),
///     rtt: Duration::from_millis(rtt_ms),
///     inflight: 20,
///     dropped: false,
/// };
/// // A second of 10 ms requests sets "normal"...
/// for i in 0..1_000 { g.on_sample(at(i, 10)); }
/// let normal = g.limit();
/// // ...then latency jumps to 100 ms: the limit falls.
/// for i in 1_000..1_500 { g.on_sample(at(i, 100)); }
/// assert!(g.limit() < normal);
/// ```
#[derive(Debug, Clone)]
pub struct Gradient2 {
    config: Gradient2Config,
    limit: f64,
    long_rtt: f64,
    updates: u32,
    window_end: Option<u64>,
    sum_rtt: f64,
    min_rtt: f64,
    samples: u32,
    max_inflight: usize,
}

impl Gradient2 {
    /// A Gradient2 limit with `config`.
    ///
    /// ```
    /// use ratelab::concurrency::{Gradient2, Gradient2Config, LimitAlgorithm};
    /// assert_eq!(Gradient2::new(Gradient2Config::default()).limit(), 20);
    /// ```
    pub fn new(config: Gradient2Config) -> Self {
        let min = config.min.max(1);
        let max = config.max.max(min);
        Self {
            config: Gradient2Config {
                min,
                max,
                smoothing: config.smoothing.clamp(f64::MIN_POSITIVE, 1.0),
                tolerance: config.tolerance.max(1.0),
                long_window: config.long_window.max(1),
                ..config
            },
            limit: config.initial.clamp(min, max) as f64,
            long_rtt: 0.0,
            updates: 0,
            window_end: None,
            sum_rtt: 0.0,
            min_rtt: f64::INFINITY,
            samples: 0,
            max_inflight: 0,
        }
    }

    /// The long-term latency, seconds (0 before the first window).
    ///
    /// ```
    /// use ratelab::concurrency::{Gradient2, Gradient2Config};
    /// assert_eq!(Gradient2::new(Gradient2Config::default()).long_rtt(), 0.0);
    /// ```
    pub fn long_rtt(&self) -> f64 {
        self.long_rtt
    }

    fn update(&mut self, short: f64, inflight: usize) {
        let c = self.config;
        // Simple average while warming up (10 updates), then an exponential average.
        self.updates = self.updates.saturating_add(1);
        let warmup = 10u32.min(c.long_window);
        self.long_rtt = if self.updates <= warmup {
            self.long_rtt + (short - self.long_rtt) / f64::from(self.updates)
        } else {
            let alpha = 2.0 / (f64::from(c.long_window) + 1.0);
            self.long_rtt + alpha * (short - self.long_rtt)
        };
        // Recovering from a slow period: let the long average come back down faster.
        if short > 0.0 && self.long_rtt / short > 2.0 {
            self.long_rtt *= 0.95;
        }
        if (inflight as f64) < self.limit / 2.0 || short <= 0.0 {
            return;
        }
        let gradient = (c.tolerance * self.long_rtt / short).clamp(0.5, 1.0);
        let estimate = self.limit * gradient + c.queue_size as f64;
        let next = self.limit * (1.0 - c.smoothing) + estimate * c.smoothing;
        self.limit = next.clamp(c.min as f64, c.max as f64);
    }
}

impl LimitAlgorithm for Gradient2 {
    fn limit(&self) -> usize {
        self.limit as usize
    }

    fn on_sample(&mut self, s: Sample) {
        let c = self.config;
        let rtt = s.rtt.as_secs_f64();
        self.sum_rtt += rtt;
        self.min_rtt = self.min_rtt.min(rtt);
        self.samples += 1;
        self.max_inflight = self.max_inflight.max(s.inflight);
        let end = *self
            .window_end
            .get_or_insert_with(|| Nanos::from_duration(c.min_window).0.saturating_add(s.now.0));
        if s.now.0 < end || self.samples < c.min_samples {
            return;
        }
        let short = self.sum_rtt / f64::from(self.samples);
        let inflight = self.max_inflight;
        // Next window: twice the fastest request, within [min_window, max_window].
        let length = Duration::try_from_secs_f64(2.0 * self.min_rtt)
            .unwrap_or(c.max_window)
            .clamp(c.min_window, c.max_window);
        self.window_end = Some(s.now.saturating_add(length).0);
        self.sum_rtt = 0.0;
        self.min_rtt = f64::INFINITY;
        self.samples = 0;
        self.max_inflight = 0;
        self.update(short, inflight);
    }
}

/// Which limit algorithm a [`ConcurrencyLimiter`] uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Concurrency {
    /// A static limit.
    Fixed(usize),
    /// Additive increase, multiplicative decrease.
    Aimd(AimdConfig),
    /// Latency gradient.
    Gradient2(Gradient2Config),
}

impl Concurrency {
    /// Short name: `fixed`, `aimd` or `gradient2`.
    ///
    /// ```
    /// use ratelab::concurrency::Concurrency;
    /// assert_eq!(Concurrency::Fixed(8).name(), "fixed");
    /// ```
    pub fn name(&self) -> &'static str {
        match self {
            Self::Fixed(_) => "fixed",
            Self::Aimd(_) => "aimd",
            Self::Gradient2(_) => "gradient2",
        }
    }

    /// A fresh algorithm with these settings.
    ///
    /// ```
    /// use ratelab::concurrency::Concurrency;
    /// assert_eq!(Concurrency::Fixed(8).build().limit(), 8);
    /// ```
    pub fn build(&self) -> Box<dyn LimitAlgorithm> {
        match *self {
            Self::Fixed(n) => Box::new(FixedLimit::new(n)),
            Self::Aimd(c) => Box::new(Aimd::new(c)),
            Self::Gradient2(c) => Box::new(Gradient2::new(c)),
        }
    }
}

/// How an admitted request ended, for [`ConcurrencyLimiter::release`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    /// It finished normally.
    Success,
    /// It timed out or failed because of overload: a sign to back off.
    Dropped,
    /// It ended for a reason that says nothing about load (for example the client went
    /// away); it frees its slot but is not learned from.
    Ignored,
}

/// Proof that a request was admitted. Give it back with [`ConcurrencyLimiter::release`].
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a ticket holds a slot until it is released"]
pub struct Ticket {
    inflight: usize,
}

impl Ticket {
    /// Requests in flight when this one was admitted, itself included.
    ///
    /// ```
    /// use ratelab::concurrency::{Completion, Concurrency, ConcurrencyLimiter};
    /// use ratelab::Nanos;
    /// use std::time::Duration;
    ///
    /// let limiter = ConcurrencyLimiter::new(Concurrency::Fixed(4));
    /// let t = limiter.try_acquire().unwrap();
    /// assert_eq!(t.inflight(), 1);
    /// limiter.release(t, Nanos(0), Duration::ZERO, Completion::Ignored);
    /// ```
    pub fn inflight(&self) -> usize {
        self.inflight
    }
}

#[derive(Debug)]
struct State {
    algorithm: Box<dyn LimitAlgorithm>,
    inflight: usize,
}

/// Counts requests in flight and sheds the ones above the limit. Thread-safe: share it
/// in an `Arc`.
#[derive(Debug)]
pub struct ConcurrencyLimiter {
    state: Mutex<State>,
}

impl ConcurrencyLimiter {
    /// A limiter using `concurrency`.
    ///
    /// ```
    /// use ratelab::concurrency::{Concurrency, ConcurrencyLimiter, Gradient2Config};
    /// let limiter = ConcurrencyLimiter::new(Concurrency::Gradient2(Gradient2Config::default()));
    /// assert_eq!(limiter.limit(), 20);
    /// ```
    pub fn new(concurrency: Concurrency) -> Self {
        Self::with_algorithm(concurrency.build())
    }

    /// A limiter using any [`LimitAlgorithm`].
    ///
    /// ```
    /// use ratelab::concurrency::{ConcurrencyLimiter, FixedLimit};
    /// let limiter = ConcurrencyLimiter::with_algorithm(Box::new(FixedLimit::new(3)));
    /// assert_eq!(limiter.limit(), 3);
    /// ```
    pub fn with_algorithm(algorithm: Box<dyn LimitAlgorithm>) -> Self {
        Self {
            state: Mutex::new(State {
                algorithm,
                inflight: 0,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admits a request if fewer than the limit are in flight; `None` means shed it.
    ///
    /// ```
    /// use ratelab::concurrency::{Concurrency, ConcurrencyLimiter};
    /// let limiter = ConcurrencyLimiter::new(Concurrency::Fixed(1));
    /// let _held = limiter.try_acquire().unwrap();
    /// assert!(limiter.try_acquire().is_none());
    /// ```
    pub fn try_acquire(&self) -> Option<Ticket> {
        let mut s = self.lock();
        if s.inflight >= s.algorithm.limit().max(1) {
            return None;
        }
        s.inflight += 1;
        Some(Ticket {
            inflight: s.inflight,
        })
    }

    /// Frees the slot of a finished request and lets the algorithm learn from it.
    ///
    /// ```
    /// use ratelab::concurrency::{AimdConfig, Completion, Concurrency, ConcurrencyLimiter};
    /// use ratelab::Nanos;
    /// use std::time::Duration;
    ///
    /// let limiter = ConcurrencyLimiter::new(Concurrency::Aimd(AimdConfig {
    ///     initial: 10,
    ///     ..AimdConfig::default()
    /// }));
    /// let t = limiter.try_acquire().unwrap();
    /// limiter.release(t, Nanos(0), Duration::from_secs(1), Completion::Dropped);
    /// assert_eq!(limiter.limit(), 9);
    /// assert_eq!(limiter.inflight(), 0);
    /// ```
    pub fn release(&self, ticket: Ticket, now: Nanos, rtt: Duration, completion: Completion) {
        let mut s = self.lock();
        s.inflight = s.inflight.saturating_sub(1);
        let dropped = match completion {
            Completion::Success => false,
            Completion::Dropped => true,
            Completion::Ignored => return,
        };
        s.algorithm.on_sample(Sample {
            now,
            rtt,
            inflight: ticket.inflight,
            dropped,
        });
    }

    /// The current limit.
    ///
    /// ```
    /// use ratelab::concurrency::{Concurrency, ConcurrencyLimiter};
    /// assert_eq!(ConcurrencyLimiter::new(Concurrency::Fixed(5)).limit(), 5);
    /// ```
    pub fn limit(&self) -> usize {
        self.lock().algorithm.limit().max(1)
    }

    /// Requests in flight now.
    ///
    /// ```
    /// use ratelab::concurrency::{Concurrency, ConcurrencyLimiter};
    /// assert_eq!(ConcurrencyLimiter::new(Concurrency::Fixed(5)).inflight(), 0);
    /// ```
    pub fn inflight(&self) -> usize {
        self.lock().inflight
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn sample(at_ms: u64, rtt_ms: u64, inflight: usize, dropped: bool) -> Sample {
        Sample {
            now: Nanos(at_ms * 1_000_000),
            rtt: ms(rtt_ms),
            inflight,
            dropped,
        }
    }

    #[test]
    fn fixed_never_moves() {
        let mut f = FixedLimit::new(7);
        f.on_sample(sample(0, 10_000, 7, true));
        assert_eq!(f.limit(), 7);
    }

    #[test]
    fn aimd_grows_only_when_used_and_backs_off_on_drops_and_slowness() {
        let mut a = Aimd::new(AimdConfig {
            initial: 10,
            latency_threshold: ms(100),
            ..AimdConfig::default()
        });
        a.on_sample(sample(0, 5, 2, false)); // 2 of 10 in use: app limited
        assert_eq!(a.limit(), 10);
        a.on_sample(sample(0, 5, 5, false));
        assert_eq!(a.limit(), 11);
        a.on_sample(sample(0, 150, 5, false)); // slower than the threshold
        assert_eq!(a.limit(), 9);
        for _ in 0..100 {
            a.on_sample(sample(0, 5, 1, true));
        }
        assert_eq!(a.limit(), 1, "never below the minimum");
        let mut capped = Aimd::new(AimdConfig {
            initial: 5,
            max: 6,
            ..AimdConfig::default()
        });
        for _ in 0..10 {
            capped.on_sample(sample(0, 5, 6, false));
        }
        assert_eq!(capped.limit(), 6);
    }

    #[test]
    fn gradient2_waits_for_a_full_window() {
        let mut g = Gradient2::new(Gradient2Config::default());
        // 9 samples over 200 ms: too few for a window.
        for i in 0..9 {
            g.on_sample(sample(i * 25, 10, 20, false));
        }
        assert_eq!(g.long_rtt(), 0.0);
        g.on_sample(sample(225, 10, 20, false));
        assert!((g.long_rtt() - 0.010).abs() < 1e-12);
    }

    #[test]
    fn gradient2_grows_while_latency_is_steady() {
        let mut g = Gradient2::new(Gradient2Config::default());
        let mut t = 0;
        for _ in 0..2_000 {
            t += 1;
            g.on_sample(sample(t, 10, g.limit(), false));
        }
        assert!(g.limit() > 20, "limit {}", g.limit());
    }

    #[test]
    fn gradient2_does_not_grow_when_app_limited() {
        let mut g = Gradient2::new(Gradient2Config::default());
        for t in 0..2_000 {
            g.on_sample(sample(t, 10, 3, false));
        }
        assert_eq!(g.limit(), 20);
    }

    #[test]
    fn gradient2_shrinks_by_at_most_half_per_window_and_recovers() {
        let mut g = Gradient2::new(Gradient2Config {
            initial: 100,
            ..Gradient2Config::default()
        });
        let mut t = 0;
        let mut run = |g: &mut Gradient2, n: u64, rtt: u64| {
            for _ in 0..n {
                t += 1;
                g.on_sample(sample(t, rtt, g.limit(), false));
            }
        };
        run(&mut g, 1_000, 10);
        let before = g.limit();
        // A sudden 10× slowdown, one window: smoothing 0.2 of a gradient of 0.5.
        run(&mut g, 101, 100);
        let after_one = g.limit();
        assert!(
            after_one as f64 >= before as f64 * 0.9 - 1.0,
            "{before} → {after_one}"
        );
        assert!(after_one < before);
        run(&mut g, 5_000, 100);
        let low = g.limit();
        assert!(low < before / 2, "{before} → {low}");
        run(&mut g, 20_000, 10);
        assert!(g.limit() > low, "recovers: {low} → {}", g.limit());
    }

    #[test]
    fn gradient2_learns_from_timeouts() {
        let mut g = Gradient2::new(Gradient2Config::default());
        for t in 0..1_000 {
            g.on_sample(sample(t, 10, 20, false));
        }
        let before = g.limit();
        for t in 1_000..3_000 {
            g.on_sample(sample(t, 200, 20, true));
        }
        assert!(g.limit() < before);
    }

    #[test]
    fn limiter_sheds_above_the_limit_and_ignores_cancelled_requests() {
        let l = ConcurrencyLimiter::new(Concurrency::Aimd(AimdConfig {
            initial: 2,
            ..AimdConfig::default()
        }));
        let a = l.try_acquire().unwrap();
        let b = l.try_acquire().unwrap();
        assert_eq!(b.inflight(), 2);
        assert!(l.try_acquire().is_none());
        l.release(a, Nanos(0), ms(1), Completion::Ignored);
        assert_eq!(l.limit(), 2, "ignored requests teach nothing");
        assert_eq!(l.inflight(), 1);
        l.release(b, Nanos(0), ms(1), Completion::Success);
        assert_eq!(l.limit(), 3);
        assert_eq!(l.inflight(), 0);
    }

    #[test]
    fn limiter_never_exceeds_its_limit_across_threads() {
        let l = Arc::new(ConcurrencyLimiter::new(Concurrency::Fixed(4)));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (l, peak) = (Arc::clone(&l), Arc::clone(&peak));
                std::thread::spawn(move || {
                    for _ in 0..10_000 {
                        if let Some(t) = l.try_acquire() {
                            peak.fetch_max(l.inflight(), std::sync::atomic::Ordering::Relaxed);
                            l.release(t, Nanos(0), Duration::ZERO, Completion::Success);
                        }
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert!(peak.load(std::sync::atomic::Ordering::Relaxed) <= 4);
        assert_eq!(l.inflight(), 0);
    }

    #[test]
    fn names_and_builds() {
        assert_eq!(Concurrency::Aimd(AimdConfig::default()).name(), "aimd");
        assert_eq!(
            Concurrency::Gradient2(Gradient2Config::default())
                .build()
                .limit(),
            20
        );
    }
}
