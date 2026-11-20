//! Overload: a backend with limited capacity, with or without a concurrency limit in
//! front of it.
//!
//! The backend has a number of workers (which can change during a run, as when
//! replicas fail or are added) and a FIFO queue. Each request needs one worker for its
//! service time. The server answers `504` when a request is not done by the deadline,
//! but the backend does not know and still does the work later: the classic setup for
//! congestion collapse, where an unlimited server ends up spending all its capacity on
//! requests nobody is waiting for.
//!
//! A [`ConcurrencyLimiter`] in front sheds requests above its limit at once (`503`)
//! and learns from every answered request.
//!
//! ```
//! use ratelab::concurrency::{Concurrency, ConcurrencyLimiter};
//! use sim::overload::{OverloadConfig, measure, requests, run};
//! use std::time::Duration;
//!
//! // 4 workers × 10 ms = 400 requests/s of capacity, offered 1,000/s.
//! let config = OverloadConfig {
//!     duration: Duration::from_secs(5),
//!     deadline: Duration::from_millis(100),
//!     service_mean: Duration::from_millis(10),
//!     load: vec![(Duration::ZERO, 1_000)],
//!     workers: vec![(Duration::ZERO, 4)],
//! };
//! let reqs = requests(1, &config);
//! let bucket = Duration::from_millis(500);
//!
//! let unlimited = measure(&config, &run(&config, &reqs, None), bucket);
//! let limiter = ConcurrencyLimiter::new(Concurrency::Fixed(4));
//! let limited = measure(&config, &run(&config, &reqs, Some(&limiter)), bucket);
//! // Without a limit the queue grows until almost everything times out.
//! assert!(limited.ok > 3 * unlimited.ok);
//! ```

use crate::SplitMix64;
use crate::scenarios::{exponential_q32, scale};
use ratelab::Nanos;
use ratelab::concurrency::{Completion, ConcurrencyLimiter, Ticket};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::time::Duration;

/// A backend, its deadline and the load offered to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverloadConfig {
    /// Length of the run.
    pub duration: Duration,
    /// The server answers `504` after this long.
    pub deadline: Duration,
    /// Mean service time of one request on one worker.
    pub service_mean: Duration,
    /// Offered load as `(time, requests per second)` points, linear in between and held
    /// after the last one. Must start at time zero.
    pub load: Vec<(Duration, u64)>,
    /// Backend workers as `(from time, workers)` steps. Must start at time zero.
    pub workers: Vec<(Duration, usize)>,
}

impl OverloadConfig {
    /// Offered requests per second at `t`.
    ///
    /// ```
    /// # use sim::overload::OverloadConfig;
    /// # use std::time::Duration;
    /// let s = Duration::from_secs;
    /// let c = OverloadConfig {
    ///     duration: s(10),
    ///     deadline: s(1),
    ///     service_mean: s(1),
    ///     load: vec![(s(0), 100), (s(10), 300)],
    ///     workers: vec![(s(0), 1)],
    /// };
    /// assert_eq!(c.rate_at(ratelab::Nanos::from_duration(s(5))), 200);
    /// ```
    pub fn rate_at(&self, t: Nanos) -> u64 {
        let t = t.0;
        let mut prev: Option<(u64, u64)> = None;
        for &(at, rate) in &self.load {
            let at = Nanos::from_duration(at).0;
            if t < at {
                return match prev {
                    Some((p_at, p_rate)) if at > p_at => {
                        let f = (t - p_at) as f64 / (at - p_at) as f64;
                        (p_rate as f64 + (rate as f64 - p_rate as f64) * f).round() as u64
                    }
                    _ => rate,
                };
            }
            prev = Some((at, rate));
        }
        prev.map_or(0, |p| p.1)
    }

    /// Backend workers at `t`.
    ///
    /// ```
    /// # use sim::overload::OverloadConfig;
    /// # use std::time::Duration;
    /// let s = Duration::from_secs;
    /// let c = OverloadConfig {
    ///     duration: s(10),
    ///     deadline: s(1),
    ///     service_mean: s(1),
    ///     load: vec![(s(0), 1)],
    ///     workers: vec![(s(0), 8), (s(5), 2)],
    /// };
    /// assert_eq!(c.workers_at(ratelab::Nanos::from_duration(s(6))), 2);
    /// ```
    pub fn workers_at(&self, t: Nanos) -> usize {
        self.workers
            .iter()
            .take_while(|(at, _)| Nanos::from_duration(*at) <= t)
            .last()
            .map_or(0, |w| w.1)
    }

    /// Requests per second the backend can complete at `t` (workers / mean service time).
    ///
    /// ```
    /// # use sim::overload::OverloadConfig;
    /// # use std::time::Duration;
    /// let c = OverloadConfig {
    ///     duration: Duration::from_secs(1),
    ///     deadline: Duration::from_secs(1),
    ///     service_mean: Duration::from_millis(20),
    ///     load: vec![(Duration::ZERO, 1)],
    ///     workers: vec![(Duration::ZERO, 16)],
    /// };
    /// assert_eq!(c.capacity_at(ratelab::Nanos(0)), 800.0);
    /// ```
    pub fn capacity_at(&self, t: Nanos) -> f64 {
        let s = self.service_mean.as_secs_f64();
        if s > 0.0 {
            self.workers_at(t) as f64 / s
        } else {
            f64::INFINITY
        }
    }
}

/// One offered request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    /// When it arrives.
    pub at: Nanos,
    /// Worker time it needs.
    pub service: Duration,
}

/// Poisson arrivals at the configured rate, each with a service time of half the mean
/// plus an exponential part (so the mean is `service_mean` and nothing is instant).
///
/// ```
/// # use sim::overload::{OverloadConfig, requests};
/// # use std::time::Duration;
/// let c = OverloadConfig {
///     duration: Duration::from_secs(10),
///     deadline: Duration::from_millis(100),
///     service_mean: Duration::from_millis(10),
///     load: vec![(Duration::ZERO, 500)],
///     workers: vec![(Duration::ZERO, 4)],
/// };
/// let r = requests(3, &c);
/// assert!((4_700..5_300).contains(&r.len()));
/// assert_eq!(r, requests(3, &c));
/// ```
pub fn requests(seed: u64, config: &OverloadConfig) -> Vec<Request> {
    let end = Nanos::from_duration(config.duration).0;
    let half = Nanos::from_duration(config.service_mean).0 / 2;
    let mut gaps = SplitMix64::new(seed);
    let mut work = SplitMix64::new(seed ^ 0x5DEE_CE66_D1CE_5EED);
    let mut now = 0u64;
    let mut out = Vec::new();
    loop {
        let rate = config.rate_at(Nanos(now));
        let gap = 1_000_000_000u64.checked_div(rate);
        now = match gap {
            Some(mean) => now.saturating_add(scale(exponential_q32(&mut gaps), mean).max(1)),
            None => now.saturating_add(1_000_000), // idle: look again in 1 ms
        };
        if now >= end {
            return out;
        }
        if gap.is_some() {
            let service = half + scale(exponential_q32(&mut work), half);
            out.push(Request {
                at: Nanos(now),
                service: Duration::from_nanos(service),
            });
        }
    }
}

/// How the server answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// `200`, within the deadline.
    Ok,
    /// `503`: shed by the concurrency limit.
    Shed,
    /// `504`: not done by the deadline.
    Timeout,
}

/// One answered request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Served {
    /// When it arrived.
    pub at: Nanos,
    /// How long the client waited for the answer.
    pub latency: Duration,
    /// The answer.
    pub status: Status,
    /// The concurrency limit when it arrived (`None` without a limiter).
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Event {
    Workers(usize),
    Arrive(usize),
    Finish(usize),
    Deadline(usize),
}

/// Runs `requests` (sorted by time) against the backend, behind `limiter` if given.
/// Returns one [`Served`] per request, in the same order.
///
/// ```
/// # use sim::overload::{OverloadConfig, Status, requests, run};
/// # use std::time::Duration;
/// let c = OverloadConfig {
///     duration: Duration::from_secs(2),
///     deadline: Duration::from_millis(100),
///     service_mean: Duration::from_millis(10),
///     load: vec![(Duration::ZERO, 50)], // far below 4 / 10 ms
///     workers: vec![(Duration::ZERO, 4)],
/// };
/// let served = run(&c, &requests(1, &c), None);
/// assert!(served.iter().all(|s| s.status == Status::Ok));
/// ```
pub fn run(
    config: &OverloadConfig,
    requests: &[Request],
    limiter: Option<&ConcurrencyLimiter>,
) -> Vec<Served> {
    let deadline = Nanos::from_duration(config.deadline).0;
    let mut heap: BinaryHeap<Reverse<(u64, u64, Event)>> = BinaryHeap::new();
    let mut seq = 0u64;
    let mut push = |heap: &mut BinaryHeap<_>, at: u64, e: Event| {
        heap.push(Reverse((at, seq, e)));
        seq += 1;
    };
    for &(at, n) in &config.workers {
        push(&mut heap, Nanos::from_duration(at).0, Event::Workers(n));
    }
    for (i, r) in requests.iter().enumerate() {
        push(&mut heap, r.at.0, Event::Arrive(i));
    }

    let mut served: Vec<Option<Served>> = vec![None; requests.len()];
    let mut tickets: Vec<Option<Ticket>> = (0..requests.len()).map(|_| None).collect();
    let mut limits: Vec<Option<usize>> = vec![None; requests.len()];
    let mut workers = 0usize;
    let mut busy = 0usize;
    let mut queue: VecDeque<usize> = VecDeque::new();

    let answer = |served: &mut Vec<Option<Served>>,
                  tickets: &mut Vec<Option<Ticket>>,
                  limits: &[Option<usize>],
                  i: usize,
                  now: u64,
                  status: Status| {
        let latency = Duration::from_nanos(now - requests[i].at.0);
        if let (Some(l), Some(t)) = (limiter, tickets[i].take()) {
            let completion = match status {
                Status::Ok => Completion::Success,
                _ => Completion::Dropped,
            };
            l.release(t, Nanos(now), latency, completion);
        }
        served[i] = Some(Served {
            at: requests[i].at,
            latency,
            status,
            limit: limits[i],
        });
    };

    while let Some(Reverse((now, _, event))) = heap.pop() {
        match event {
            Event::Workers(n) => workers = n,
            Event::Arrive(i) => {
                limits[i] = limiter.map(ConcurrencyLimiter::limit);
                match limiter.map(ConcurrencyLimiter::try_acquire) {
                    Some(None) => {
                        answer(&mut served, &mut tickets, &limits, i, now, Status::Shed);
                        continue;
                    }
                    Some(t) => tickets[i] = t,
                    None => {}
                }
                queue.push_back(i);
                push(&mut heap, now.saturating_add(deadline), Event::Deadline(i));
            }
            Event::Finish(i) => {
                busy -= 1;
                if served[i].is_none() {
                    answer(&mut served, &mut tickets, &limits, i, now, Status::Ok);
                }
            }
            Event::Deadline(i) => {
                if served[i].is_none() {
                    answer(&mut served, &mut tickets, &limits, i, now, Status::Timeout);
                }
            }
        }
        // The backend does not know about deadlines: queued work is always done.
        while busy < workers {
            let Some(next) = queue.pop_front() else { break };
            busy += 1;
            let done = Nanos(now).saturating_add(requests[next].service).0;
            push(&mut heap, done, Event::Finish(next));
        }
    }
    served
        .into_iter()
        .zip(requests)
        .map(|(s, r)| {
            s.unwrap_or(Served {
                at: r.at,
                latency: config.deadline,
                status: Status::Timeout,
                limit: None,
            })
        })
        .collect()
}

/// One time bucket of a run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bucket {
    /// Start of the bucket, seconds.
    pub start_secs: f64,
    /// Requests that arrived in it.
    pub offered: u64,
    /// Of those, answered `200`.
    pub ok: u64,
    /// Of those, shed.
    pub shed: u64,
    /// Of those, timed out.
    pub timeout: u64,
    /// The most that could have succeeded: `min(offered, capacity × bucket length)`.
    pub possible: f64,
    /// Mean concurrency limit seen by arrivals (`None` without a limiter).
    pub limit: Option<f64>,
    /// p99 latency of the `200`s, ms (0 if none).
    pub p99_ms: f64,
}

/// What a run achieved.
#[derive(Debug, Clone, PartialEq)]
pub struct OverloadMetrics {
    /// Requests offered.
    pub offered: u64,
    /// Answered `200` within the deadline.
    pub ok: u64,
    /// Shed (`503`).
    pub shed: u64,
    /// Timed out (`504`).
    pub timeout: u64,
    /// `200`s per second over the run.
    pub goodput_rps: f64,
    /// `200`s as a % of what could have succeeded (the buckets' `possible`).
    pub goodput_pct: f64,
    /// Median latency of the `200`s, ms.
    pub p50_ms: f64,
    /// p99 latency of the `200`s, ms.
    pub p99_ms: f64,
    /// Per-bucket detail, for charts.
    pub series: Vec<Bucket>,
}

fn percentile_ms(sorted_ns: &[u64], p: f64) -> f64 {
    if sorted_ns.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted_ns.len() as f64).ceil() as usize;
    sorted_ns[rank.clamp(1, sorted_ns.len()) - 1] as f64 / 1e6
}

/// Measures `served` (from [`run`] or from a real server) in buckets of `bucket`.
///
/// ```
/// # use sim::overload::{OverloadConfig, measure, requests, run};
/// # use std::time::Duration;
/// let c = OverloadConfig {
///     duration: Duration::from_secs(2),
///     deadline: Duration::from_millis(100),
///     service_mean: Duration::from_millis(10),
///     load: vec![(Duration::ZERO, 50)],
///     workers: vec![(Duration::ZERO, 4)],
/// };
/// let m = measure(&c, &run(&c, &requests(1, &c), None), Duration::from_millis(500));
/// assert_eq!(m.ok, m.offered);
/// assert_eq!(m.series.len(), 4);
/// assert!(m.goodput_pct > 99.0);
/// ```
pub fn measure(config: &OverloadConfig, served: &[Served], bucket: Duration) -> OverloadMetrics {
    let width = Nanos::from_duration(bucket).0.max(1);
    let span = Nanos::from_duration(config.duration).0;
    let n = span.div_ceil(width) as usize;
    let mut series: Vec<Bucket> = (0..n)
        .map(|b| {
            let start = b as u64 * width;
            let len = width.min(span - start) as f64 / 1e9;
            Bucket {
                start_secs: start as f64 / 1e9,
                offered: 0,
                ok: 0,
                shed: 0,
                timeout: 0,
                possible: config.capacity_at(Nanos(start)) * len,
                limit: None,
                p99_ms: 0.0,
            }
        })
        .collect();
    let mut lat: Vec<Vec<u64>> = vec![Vec::new(); n];
    let mut limit_sum = vec![(0.0f64, 0u64); n];
    let mut all_ok = Vec::new();
    for s in served {
        let b = ((s.at.0 / width) as usize).min(n.saturating_sub(1));
        let Some(bk) = series.get_mut(b) else {
            continue;
        };
        bk.offered += 1;
        match s.status {
            Status::Ok => {
                bk.ok += 1;
                let ns = u64::try_from(s.latency.as_nanos()).unwrap_or(u64::MAX);
                lat[b].push(ns);
                all_ok.push(ns);
            }
            Status::Shed => bk.shed += 1,
            Status::Timeout => bk.timeout += 1,
        }
        if let Some(l) = s.limit {
            limit_sum[b].0 += l as f64;
            limit_sum[b].1 += 1;
        }
    }
    let mut possible = 0.0;
    for (b, bk) in series.iter_mut().enumerate() {
        bk.possible = bk.possible.min(bk.offered as f64);
        possible += bk.possible;
        lat[b].sort_unstable();
        bk.p99_ms = percentile_ms(&lat[b], 99.0);
        if limit_sum[b].1 > 0 {
            bk.limit = Some(limit_sum[b].0 / limit_sum[b].1 as f64);
        }
    }
    all_ok.sort_unstable();
    let count = |st: Status| served.iter().filter(|s| s.status == st).count() as u64;
    let ok = count(Status::Ok);
    let secs = config.duration.as_secs_f64();
    OverloadMetrics {
        offered: served.len() as u64,
        ok,
        shed: count(Status::Shed),
        timeout: count(Status::Timeout),
        goodput_rps: if secs > 0.0 { ok as f64 / secs } else { 0.0 },
        goodput_pct: if possible > 0.0 {
            ok as f64 * 100.0 / possible
        } else {
            0.0
        },
        p50_ms: percentile_ms(&all_ok, 50.0),
        p99_ms: percentile_ms(&all_ok, 99.0),
        series,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratelab::concurrency::{AimdConfig, Concurrency, Gradient2Config};

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn config(load: u64, workers: usize) -> OverloadConfig {
        OverloadConfig {
            duration: Duration::from_secs(20),
            deadline: ms(200),
            service_mean: ms(20),
            load: vec![(Duration::ZERO, load)],
            workers: vec![(Duration::ZERO, workers)],
        }
    }

    fn served(status: Status, at_ms: u64, latency_ms: u64) -> Served {
        Served {
            at: Nanos(at_ms * 1_000_000),
            latency: ms(latency_ms),
            status,
            limit: Some(4),
        }
    }

    #[test]
    fn service_times_average_the_mean() {
        let r = requests(9, &config(1_000, 16));
        let mean = r.iter().map(|x| x.service.as_secs_f64()).sum::<f64>() / r.len() as f64;
        assert!((mean - 0.020).abs() < 0.001, "{mean}");
        assert!(r.iter().all(|x| x.service >= ms(10)));
    }

    #[test]
    fn load_ramps_linearly() {
        let mut c = config(0, 1);
        c.load = vec![(Duration::ZERO, 100), (Duration::from_secs(10), 1_100)];
        let at = |s: u64| Nanos(s * 1_000_000_000);
        assert_eq!(c.rate_at(at(0)), 100);
        assert_eq!(c.rate_at(at(5)), 600);
        assert_eq!(c.rate_at(at(15)), 1_100);
        let r = requests(1, &c);
        let first = r.iter().filter(|x| x.at < at(5)).count();
        assert!(first * 2 < r.len() - first);
    }

    #[test]
    fn without_a_limit_overload_collapses_goodput() {
        let c = config(1_200, 16); // 1.5 × the 800/s capacity
        let m = measure(&c, &run(&c, &requests(1, &c), None), ms(500));
        assert_eq!(m.shed, 0);
        assert!(m.goodput_pct < 25.0, "{}", m.goodput_pct);
        // The last seconds are almost all timeouts.
        let tail = &m.series[m.series.len() - 4..];
        assert!(tail.iter().all(|b| b.ok * 10 < b.offered));
    }

    #[test]
    fn a_limit_at_the_worker_count_keeps_goodput_and_latency() {
        let c = config(1_200, 16);
        let l = ConcurrencyLimiter::new(Concurrency::Fixed(16));
        let m = measure(&c, &run(&c, &requests(1, &c), Some(&l)), ms(500));
        assert_eq!(m.timeout, 0);
        assert!(m.goodput_pct > 85.0, "{}", m.goodput_pct);
        assert!(m.p99_ms < 120.0, "{}", m.p99_ms);
        assert_eq!(l.inflight(), 0, "every ticket was released");
    }

    #[test]
    fn adaptive_limits_avoid_collapse() {
        let c = config(1_200, 16);
        for algo in [
            Concurrency::Aimd(AimdConfig {
                latency_threshold: c.deadline,
                ..AimdConfig::default()
            }),
            Concurrency::Gradient2(Gradient2Config::default()),
        ] {
            let l = ConcurrencyLimiter::new(algo);
            let m = measure(&c, &run(&c, &requests(1, &c), Some(&l)), ms(500));
            assert!(m.goodput_pct > 50.0, "{}: {}", algo.name(), m.goodput_pct);
        }
    }

    #[test]
    fn runs_are_deterministic() {
        let c = config(1_200, 16);
        let r = requests(4, &c);
        let a = run(
            &c,
            &r,
            Some(&ConcurrencyLimiter::new(Concurrency::Gradient2(
                Gradient2Config::default(),
            ))),
        );
        let b = run(
            &c,
            &r,
            Some(&ConcurrencyLimiter::new(Concurrency::Gradient2(
                Gradient2Config::default(),
            ))),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn workers_can_change_mid_run() {
        let mut c = config(600, 16);
        c.workers = vec![(Duration::ZERO, 16), (Duration::from_secs(10), 4)];
        let m = measure(&c, &run(&c, &requests(2, &c), None), ms(1_000));
        assert!(m.series[..10].iter().all(|b| b.timeout == 0));
        assert!(m.series[12..].iter().all(|b| b.ok < b.offered / 2));
        assert_eq!(
            c.capacity_at(Nanos::from_duration(Duration::from_secs(11))),
            200.0
        );
    }

    #[test]
    fn measure_counts_and_buckets() {
        let c = config(10, 1);
        let s = [
            served(Status::Ok, 100, 10),
            served(Status::Ok, 200, 30),
            served(Status::Shed, 600, 0),
            served(Status::Timeout, 19_900, 200),
        ];
        let m = measure(&c, &s, Duration::from_secs(1));
        assert_eq!((m.offered, m.ok, m.shed, m.timeout), (4, 2, 1, 1));
        assert_eq!(m.series.len(), 20);
        assert_eq!(m.series[0].offered, 3);
        assert_eq!(m.series[0].possible, 3.0); // capacity 50/s, but only 3 offered
        assert_eq!(m.series[0].p99_ms, 30.0);
        assert_eq!(m.series[0].limit, Some(4.0));
        assert_eq!(m.p50_ms, 10.0);
        assert!((m.goodput_pct - 50.0).abs() < 1e-9);
    }
}
