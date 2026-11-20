//! Shared conformance suite: one behaviour contract, run against every algorithm in
//! `Algorithm::ALL`. Time comes from a `ManualClock`; nothing sleeps.

use drl_core::{Algorithm, Cost, ManualClock, RateLimitConfig, RateLimiter, Tokens};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const ONE_NS: Duration = Duration::from_nanos(1);

fn build(
    algorithm: Algorithm,
    limit: u64,
    window: Duration,
) -> (Box<dyn RateLimiter>, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new());
    let config = RateLimitConfig::new(limit, window).unwrap();
    (algorithm.build(config, Arc::clone(&clock)), clock)
}

/// The most units an algorithm may admit in any span of length `window`.
/// Exhaustive on purpose: a new algorithm must state its bound here.
fn max_per_window(algorithm: Algorithm, limit: u64) -> u64 {
    match algorithm {
        Algorithm::SlidingWindow => limit,
        // A full bucket (or the end of one fixed window) plus one window's worth more.
        Algorithm::TokenBucket | Algorithm::LeakyBucket | Algorithm::FixedWindow => 2 * limit,
    }
}

/// Small deterministic PRNG (xorshift64*), so the suite needs no extra dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn fresh_key_gets_exactly_the_limit_as_a_burst() {
    for a in Algorithm::ALL {
        let (l, _clock) = build(a, 5, Duration::from_secs(1));
        for i in 0..5u64 {
            let d = l.check("k", Cost::ONE);
            assert!(d.allowed, "{a}: request {i} should be allowed");
            assert_eq!(d.remaining, Tokens(4 - i), "{a}: remaining after {i}");
            assert_eq!(d.retry_after, None, "{a}: allowed has no retry_after");
        }
        let d = l.check("k", Cost::ONE);
        assert!(!d.allowed, "{a}: request over the limit must be denied");
        assert_eq!(d.remaining, Tokens(0), "{a}");
        assert!(
            d.retry_after.is_some(),
            "{a}: denied must say when to retry"
        );
    }
}

#[test]
fn keys_are_independent() {
    for a in Algorithm::ALL {
        let (l, _clock) = build(a, 2, Duration::from_secs(1));
        assert!(l.check("a", Cost(2)).allowed, "{a}");
        assert!(!l.check("a", Cost::ONE).allowed, "{a}");
        assert!(
            l.check("b", Cost(2)).allowed,
            "{a}: key b has its own quota"
        );
    }
}

#[test]
fn reset_restores_the_full_quota() {
    for a in Algorithm::ALL {
        let (l, _clock) = build(a, 3, Duration::from_secs(1));
        assert!(l.check("k", Cost(3)).allowed, "{a}");
        assert!(!l.check("k", Cost::ONE).allowed, "{a}");
        l.reset("k");
        assert!(
            l.check("k", Cost(3)).allowed,
            "{a}: reset gives a full quota"
        );
    }
}

#[test]
fn cost_above_the_limit_is_never_admitted() {
    for a in Algorithm::ALL {
        let (l, clock) = build(a, 4, Duration::from_secs(1));
        for _ in 0..3 {
            let d = l.check("k", Cost(5));
            assert!(!d.allowed, "{a}");
            assert_eq!(d.retry_after, None, "{a}: no retry can ever succeed");
            clock.advance(Duration::from_secs(10));
        }
        assert!(
            l.check("k", Cost(4)).allowed,
            "{a}: a cost of exactly the limit fits"
        );
    }
}

#[test]
fn zero_cost_is_always_allowed_and_charges_nothing() {
    for a in Algorithm::ALL {
        let (l, _clock) = build(a, 2, Duration::from_secs(1));
        assert!(l.check("k", Cost(2)).allowed, "{a}");
        let d = l.check("k", Cost(0));
        assert!(d.allowed, "{a}: zero cost passes even when exhausted");
        assert_eq!(d.remaining, Tokens(0), "{a}");
        assert!(
            !l.check("k", Cost::ONE).allowed,
            "{a}: and it charged nothing"
        );
    }
}

#[test]
fn retry_after_is_exact() {
    // Waiting retry_after - 1ns is not enough; waiting retry_after is.
    for a in Algorithm::ALL {
        for seed in 1..=20u64 {
            // Limits that do not divide the window, so rounding errors cannot hide.
            let limit = 3 + seed % 11;
            let (l, clock) = build(a, limit, Duration::from_nanos(100_000_007));
            let mut rng = Rng(seed);
            let mut denials = 0;
            for _ in 0..300 {
                clock.advance(Duration::from_nanos(rng.below(3_000_000)));
                let cost = Cost(1 + rng.below(limit));
                let d = l.check("k", cost);
                if d.allowed {
                    continue;
                }
                denials += 1;
                let wait = d
                    .retry_after
                    .unwrap_or_else(|| panic!("{a}: cost <= limit has a retry"));
                assert!(wait > Duration::ZERO, "{a}: denied means a positive wait");

                clock.advance(wait - ONE_NS);
                assert!(
                    !l.check("k", cost).allowed,
                    "{a} seed {seed}: allowed 1ns before retry_after"
                );
                clock.advance(ONE_NS);
                assert!(
                    l.check("k", cost).allowed,
                    "{a} seed {seed}: denied at retry_after"
                );
            }
            assert!(denials > 0, "{a} seed {seed}: scenario never hit the limit");
        }
    }
}

#[test]
fn waiting_reset_after_restores_the_full_quota() {
    for a in Algorithm::ALL {
        let (l, clock) = build(a, 8, Duration::from_millis(100));
        let mut rng = Rng(7);
        for _ in 0..200 {
            clock.advance(Duration::from_nanos(rng.below(20_000_000)));
            let d = l.check("k", Cost(1 + rng.below(8)));
            clock.advance(d.reset_after);
            let full = l.check("k", Cost(0));
            assert_eq!(
                full.remaining,
                Tokens(8),
                "{a}: full quota after reset_after"
            );
            assert_eq!(
                full.reset_after,
                Duration::ZERO,
                "{a}: nothing left to reset"
            );
        }
    }
}

#[test]
fn an_idle_window_restores_the_full_quota() {
    for a in Algorithm::ALL {
        let (l, clock) = build(a, 6, Duration::from_millis(250));
        assert!(l.check("k", Cost(6)).allowed, "{a}");
        // Fixed windows are aligned to the clock origin, so one window length from the
        // start always lands in the next window.
        clock.advance(Duration::from_millis(250));
        assert!(
            l.check("k", Cost(6)).allowed,
            "{a}: full quota after one idle window"
        );
    }
}

#[test]
fn never_admits_more_than_its_bound_in_any_window() {
    let window = Duration::from_millis(50);
    let w = window.as_nanos() as u64;
    for a in Algorithm::ALL {
        for seed in 1..=10u64 {
            let limit = 1 + seed % 7;
            let (l, clock) = build(a, limit, window);
            let mut rng = Rng(seed * 31);
            let mut admitted: Vec<(u64, u64)> = Vec::new();
            let mut now = 0u64;
            for _ in 0..3_000 {
                // Mix tight bursts with gaps around a window length.
                let gap = if rng.below(4) == 0 {
                    rng.below(2 * w)
                } else {
                    rng.below(w / 20)
                };
                clock.advance(Duration::from_nanos(gap));
                now += gap;
                let cost = 1 + rng.below(limit);
                if l.check("k", Cost(cost)).allowed {
                    admitted.push((now, cost));
                }
            }
            // Two pointers: total admitted in every span [t, t + window).
            let bound = max_per_window(a, limit);
            let (mut lo, mut sum) = (0usize, 0u64);
            for hi in 0..admitted.len() {
                sum += admitted[hi].1;
                while admitted[hi].0 - admitted[lo].0 >= w {
                    sum -= admitted[lo].1;
                    lo += 1;
                }
                assert!(
                    sum <= bound,
                    "{a} seed {seed}: admitted {sum} in one window, bound {bound}"
                );
            }
        }
    }
}

#[test]
fn same_inputs_give_same_decisions() {
    for a in Algorithm::ALL {
        let run = || {
            let (l, clock) = build(a, 5, Duration::from_millis(10));
            let mut rng = Rng(99);
            (0..1_000)
                .map(|_| {
                    clock.advance(Duration::from_nanos(rng.below(1_000_000)));
                    let key = ["a", "b", "c"][rng.below(3) as usize];
                    l.check(key, Cost(1 + rng.below(5)))
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run(), "{a}: decisions must be deterministic");
    }
}

#[test]
fn concurrent_checks_never_over_admit() {
    // Time is frozen, so no refill: exactly `limit` units can be admitted in total,
    // however the threads interleave.
    const THREADS: u64 = 8;
    const CHECKS_PER_THREAD: u64 = 2_000;
    const LIMIT: u64 = 5_000;
    for a in Algorithm::ALL {
        let (l, _clock) = build(a, LIMIT, Duration::from_secs(3600));
        let l: Arc<dyn RateLimiter> = Arc::from(l);
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let l = Arc::clone(&l);
                thread::spawn(move || {
                    (0..CHECKS_PER_THREAD)
                        .filter(|_| l.check("shared", Cost::ONE).allowed)
                        .count() as u64
                })
            })
            .collect();
        let admitted: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(
            admitted, LIMIT,
            "{a}: lost or doubled updates under contention"
        );
    }
}
