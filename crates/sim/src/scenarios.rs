//! Time-based traffic scenarios: steady Poisson, on/off, heavy-tailed (Pareto),
//! diurnal ramp and flash crowd.
//!
//! Each generator produces arrivals for `duration` of scenario time, sorted by time.
//! Randomness comes from [`SplitMix64`] and every formula is fixed-point integer
//! math (no `ln`, `exp` or `sin` from the platform's float library), so a seed gives the
//! same arrivals on every platform, including WebAssembly.

use crate::{Arrival, SplitMix64};
use drl_core::{Cost, Nanos};
use std::time::Duration;

/// Fractional bits of the fixed-point values below.
const Q: u32 = 32;
const ONE: u64 = 1 << Q;
/// ln(2) in Q32, rounded to nearest.
const LN2_Q32: u128 = 2_977_044_472;

/// log2(x) in Q32 for `x >= 1`, exact to the last bit computed.
fn log2_q32(x: u64) -> u64 {
    debug_assert!(x >= 1);
    let int = 63 - u64::from(x.leading_zeros());
    // Mantissa in [1, 2) as Q62.
    let mut m: u128 = u128::from(x) << (62 - int);
    let mut frac = 0u64;
    for bit in (0..Q).rev() {
        m = (m * m) >> 62;
        if m >= 2 << 62 {
            m >>= 1;
            frac |= 1 << bit;
        }
    }
    (int << Q) | frac
}

/// A standard exponential variate (mean 1) in Q32: `-ln(U)` with `U` uniform in (0, 1].
fn exponential_q32(rng: &mut SplitMix64) -> u64 {
    let u = (rng.next_u64() >> Q).max(1); // 1..=2^32-1, as U × 2^32
    let neg_log2 = (u64::from(Q) << Q) - log2_q32(u);
    ((u128::from(neg_log2) * LN2_Q32) >> Q) as u64
}

/// A Pareto variate with shape 2 and mean 1, in Q32: `1 / (2 √U)`.
///
/// Shape 2 has a finite mean and infinite variance: most gaps are short and a few are
/// very long. `U` is drawn from 40 bits, which caps the variate at about 2^19.
fn pareto2_q32(rng: &mut SplitMix64) -> u64 {
    let u = (rng.next_u64() >> 24).max(1); // U × 2^40
    // 1/√U in Q32 is √(2^40 / u) × 2^32 = √(2^104 / u).
    let inv_sqrt = ((1u128 << 104) / u128::from(u)).isqrt();
    u64::try_from(inv_sqrt / 2).unwrap_or(u64::MAX)
}

/// Scales a Q32 variate by a mean gap in nanoseconds.
fn scale(variate_q32: u64, mean_gap_ns: u64) -> u64 {
    u64::try_from((u128::from(variate_q32) * u128::from(mean_gap_ns)) >> Q).unwrap_or(u64::MAX)
}

/// Shared loop: draw gaps until `duration`, each from `gap(rng, now)`.
fn generate(
    seed: u64,
    duration: Duration,
    keys: u32,
    mut gap: impl FnMut(&mut SplitMix64, u64) -> u64,
) -> Vec<Arrival> {
    let end = Nanos::from_duration(duration).0;
    let mut gaps = SplitMix64::new(seed);
    let mut pick = SplitMix64::new(seed ^ 0xA5A5_A5A5_A5A5_A5A5);
    let mut now = 0u64;
    let mut out = Vec::new();
    loop {
        now = now.saturating_add(gap(&mut gaps, now).max(1));
        if now >= end {
            return out;
        }
        out.push(Arrival {
            at: Nanos(now),
            key: pick.below(u64::from(keys.max(1))) as u32,
            cost: Cost::ONE,
        });
    }
}

/// Steady Poisson traffic: exponential gaps with mean `mean_gap`, uniform over `keys`.
///
/// ```
/// use std::time::Duration;
///
/// let a = sim::poisson(7, Duration::from_secs(10), Duration::from_millis(1), 4);
/// assert!((9_500..10_500).contains(&a.len())); // about 10 s / 1 ms
/// assert_eq!(a, sim::poisson(7, Duration::from_secs(10), Duration::from_millis(1), 4));
/// ```
pub fn poisson(seed: u64, duration: Duration, mean_gap: Duration, keys: u32) -> Vec<Arrival> {
    let mean = Nanos::from_duration(mean_gap).0;
    generate(seed, duration, keys, |rng, _| {
        scale(exponential_q32(rng), mean)
    })
}

/// Heavy-tailed traffic: Pareto (shape 2) gaps with mean `mean_gap`.
///
/// ```
/// use std::time::Duration;
///
/// let a = sim::pareto(3, Duration::from_secs(20), Duration::from_millis(1), 1);
/// assert!((18_000..22_000).contains(&a.len()));
/// ```
pub fn pareto(seed: u64, duration: Duration, mean_gap: Duration, keys: u32) -> Vec<Arrival> {
    let mean = Nanos::from_duration(mean_gap).0;
    generate(seed, duration, keys, |rng, _| scale(pareto2_q32(rng), mean))
}

/// On/off traffic: Poisson at `mean_gap` for `on`, then silence for `off`, repeated.
///
/// ```
/// use std::time::Duration;
///
/// let ms = Duration::from_millis;
/// let a = sim::on_off(1, Duration::from_secs(4), ms(1), ms(500), ms(500), 2);
/// // Nothing arrives in the first off period (500 ms .. 1000 ms).
/// assert!(!a.iter().any(|x| (500_000_000..1_000_000_000).contains(&x.at.0)));
/// ```
pub fn on_off(
    seed: u64,
    duration: Duration,
    mean_gap: Duration,
    on: Duration,
    off: Duration,
    keys: u32,
) -> Vec<Arrival> {
    let mean = Nanos::from_duration(mean_gap).0;
    let on = Nanos::from_duration(on).0.max(1);
    let period = on.saturating_add(Nanos::from_duration(off).0);
    generate(seed, duration, keys, |rng, now| {
        let next = now.saturating_add(scale(exponential_q32(rng), mean).max(1));
        let phase = next % period;
        if phase < on {
            next - now
        } else {
            // Skip to the start of the next on period.
            next - phase + period - now
        }
    })
}

/// Diurnal ramp: Poisson traffic whose rate rises linearly from `trough_gap` spacing to
/// `peak_gap` spacing at the middle of each `period` and back (a triangle wave, so the
/// shape is exact in integers).
///
/// ```
/// use std::time::Duration;
///
/// let ms = Duration::from_millis;
/// let a = sim::diurnal(5, Duration::from_secs(10), ms(10), ms(1), Duration::from_secs(10), 1);
/// let in_span = |lo: u64, hi: u64| a.iter().filter(|x| (lo..hi).contains(&x.at.0)).count();
/// // Busier around the peak (t = 5 s) than at the start.
/// assert!(in_span(4_500_000_000, 5_500_000_000) > 3 * in_span(0, 1_000_000_000));
/// ```
pub fn diurnal(
    seed: u64,
    duration: Duration,
    trough_gap: Duration,
    peak_gap: Duration,
    period: Duration,
    keys: u32,
) -> Vec<Arrival> {
    let period = Nanos::from_duration(period).0.max(2);
    let half = period / 2;
    // Interpolate the rate (1 / gap), not the gap, so the ramp is linear in requests/s.
    let trough_rate = rate_q32(Nanos::from_duration(trough_gap).0);
    let peak_rate = rate_q32(Nanos::from_duration(peak_gap).0);
    generate(seed, duration, keys, |rng, now| {
        let phase = now % period;
        let up = if phase < half { phase } else { period - phase };
        let rate = trough_rate
            + ((u128::from(peak_rate.saturating_sub(trough_rate)) * u128::from(up))
                / u128::from(half)) as u64;
        scale(exponential_q32(rng), gap_from_rate(rate))
    })
}

/// Flash crowd: steady Poisson at `mean_gap` over `keys` clients, except during
/// `spike_start..spike_start + spike_len`, when the rate is multiplied by `factor` and
/// arrivals come from `factor` times as many clients (new clients join).
///
/// ```
/// use std::time::Duration;
///
/// let s = Duration::from_secs;
/// let a = sim::flash_crowd(2, s(6), Duration::from_millis(1), 10, s(2), s(2), 10);
/// let in_span = |lo: u64, hi: u64| a.iter().filter(|x| (lo..hi).contains(&x.at.0)).count();
/// assert!(in_span(2_000_000_000, 4_000_000_000) > 8 * in_span(0, 2_000_000_000));
/// assert!(a.iter().any(|x| x.key >= 10)); // new clients during the spike
/// ```
pub fn flash_crowd(
    seed: u64,
    duration: Duration,
    mean_gap: Duration,
    keys: u32,
    spike_start: Duration,
    spike_len: Duration,
    factor: u32,
) -> Vec<Arrival> {
    let factor = factor.max(1);
    let end_all = Nanos::from_duration(duration).0;
    let start = Nanos::from_duration(spike_start).0.min(end_all);
    let spike_end = start
        .saturating_add(Nanos::from_duration(spike_len).0)
        .min(end_all);
    let mean = Nanos::from_duration(mean_gap).0;
    let base = poisson(seed, duration, mean_gap, keys);
    if factor == 1 || spike_end <= start {
        return base;
    }
    // Extra traffic at (factor - 1) × the base rate, from clients that were not there
    // before: keys `keys..keys × factor`.
    let base_keys = keys.max(1);
    let new_keys = base_keys.saturating_mul(factor - 1);
    let spike_mean = mean / u64::from(factor - 1);
    let spike = generate(
        seed.wrapping_add(1),
        Duration::from_nanos(spike_end - start),
        new_keys,
        |rng, _| scale(exponential_q32(rng), spike_mean),
    );
    merge([
        base,
        spike
            .into_iter()
            .map(|a| Arrival {
                at: Nanos(a.at.0 + start),
                key: base_keys + a.key,
                ..a
            })
            .collect(),
    ])
}

/// Requests per nanosecond in Q32 for a gap in nanoseconds.
fn rate_q32(gap_ns: u64) -> u64 {
    ONE / gap_ns.max(1)
}

fn gap_from_rate(rate_q32: u64) -> u64 {
    ONE / rate_q32.max(1)
}

/// Merges arrival lists into one list sorted by time (stable for equal times).
///
/// ```
/// use drl_core::{Cost, Nanos};
/// use sim::{merge, Arrival};
///
/// let a = vec![Arrival { at: Nanos(5), key: 0, cost: Cost::ONE }];
/// let b = vec![Arrival { at: Nanos(2), key: 1, cost: Cost::ONE }];
/// let m = merge([a, b]);
/// assert_eq!(m[0].key, 1);
/// ```
pub fn merge(lists: impl IntoIterator<Item = Vec<Arrival>>) -> Vec<Arrival> {
    let mut all: Vec<Arrival> = lists.into_iter().flatten().collect();
    all.sort_by_key(|a| a.at);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mean_gap_ns(a: &[Arrival]) -> f64 {
        a.last().map_or(0.0, |l| l.at.0 as f64 / a.len() as f64)
    }

    #[test]
    fn log2_is_exact_on_powers_of_two_and_close_elsewhere() {
        assert_eq!(log2_q32(1), 0);
        assert_eq!(log2_q32(1 << 20), 20 << Q);
        let three = log2_q32(3) as f64 / ONE as f64;
        assert!((three - 3f64.log2()).abs() < 1e-8, "{three}");
    }

    #[test]
    fn exponential_has_mean_one() {
        let mut rng = SplitMix64::new(11);
        let n = 200_000;
        let sum: u128 = (0..n).map(|_| u128::from(exponential_q32(&mut rng))).sum();
        let mean = sum as f64 / n as f64 / ONE as f64;
        assert!((mean - 1.0).abs() < 0.01, "{mean}");
    }

    #[test]
    fn pareto_has_mean_one_and_a_heavy_tail() {
        let mut rng = SplitMix64::new(12);
        let n = 400_000;
        let draws: Vec<u64> = (0..n).map(|_| pareto2_q32(&mut rng)).collect();
        let mean = draws.iter().map(|&d| u128::from(d)).sum::<u128>() as f64 / n as f64;
        let mean = mean / ONE as f64;
        // Infinite variance: the sample mean converges slowly.
        assert!((mean - 1.0).abs() < 0.05, "{mean}");
        let max = *draws.iter().max().unwrap() as f64 / ONE as f64;
        assert!(max > 100.0, "{max}");
        // Minimum of a shape-2, mean-1 Pareto is 0.5.
        assert!(draws.iter().all(|&d| d >= ONE / 2 - 1));
    }

    #[test]
    fn generators_are_deterministic_and_sorted() {
        let s = Duration::from_secs(2);
        let ms = Duration::from_millis(1);
        let all = [
            poisson(1, s, ms, 3),
            pareto(1, s, ms, 3),
            on_off(1, s, ms, ms * 100, ms * 100, 3),
            diurnal(1, s, ms * 5, ms, s, 3),
            flash_crowd(1, s, ms, 3, ms * 500, ms * 500, 5),
        ];
        for a in &all {
            assert!(!a.is_empty());
            assert!(a.windows(2).all(|w| w[0].at <= w[1].at));
            assert!(a.iter().all(|x| x.at < Nanos::from_duration(s)));
        }
        assert_eq!(all[4], flash_crowd(1, s, ms, 3, ms * 500, ms * 500, 5));
        assert_ne!(all[0], poisson(2, s, ms, 3));
    }

    #[test]
    fn poisson_mean_gap_matches() {
        let a = poisson(4, Duration::from_secs(100), Duration::from_millis(1), 1);
        let mean = mean_gap_ns(&a);
        assert!((mean - 1e6).abs() < 1e4, "{mean}");
    }

    #[test]
    fn flash_crowd_multiplies_the_rate_during_the_spike() {
        let s = Duration::from_secs;
        let a = flash_crowd(9, s(30), Duration::from_millis(1), 5, s(10), s(10), 5);
        let count = |lo: u64, hi: u64| {
            a.iter()
                .filter(|x| (lo * 1_000_000_000..hi * 1_000_000_000).contains(&x.at.0))
                .count() as f64
        };
        let ratio = count(10, 20) / count(0, 10);
        assert!((ratio - 5.0).abs() < 0.25, "{ratio}");
    }

    #[test]
    fn merge_keeps_every_arrival() {
        let s = Duration::from_secs(1);
        let ms = Duration::from_millis(1);
        let m = merge([poisson(1, s, ms, 1), poisson(2, s, ms, 1)]);
        assert_eq!(
            m.len(),
            poisson(1, s, ms, 1).len() + poisson(2, s, ms, 1).len()
        );
        assert!(m.windows(2).all(|w| w[0].at <= w[1].at));
    }
}
