//! Named traffic patterns at a load relative to a limit: what the playground and the
//! `ratelab` CLI mean by "bursty at 2× the limit".

use crate::Arrival;
use ratelab::{Cost, Nanos, RateLimitConfig};
use std::time::Duration;

/// Every pattern [`generate`] accepts.
pub const PATTERNS: [&str; 6] = [
    "steady",
    "bursty",
    "heavy_tailed",
    "diurnal",
    "flash_crowd",
    "boundary_attack",
];

/// The fixed-window boundary attack repeated every two windows: `limit` requests just
/// before a window edge and `limit` more 2 ms after it.
fn repeated_attack(config: RateLimitConfig, duration: Duration) -> Vec<Arrival> {
    let window = config.window_nanos();
    let end = Nanos::from_duration(duration).0;
    let gap = 2_000_000u64.min(window / 2);
    let mut out = Vec::new();
    let mut edge = window;
    while edge < end {
        for at in [edge.saturating_sub(gap / 2 + 1), edge + gap / 2] {
            out.extend((0..config.limit()).map(|_| Arrival {
                at: Nanos(at),
                key: 0,
                cost: Cost::ONE,
            }));
        }
        edge += 2 * window;
    }
    out
}

/// `pattern` for `duration` over `keys` keys, offering `load` × the limit per key.
///
/// - `steady`: Poisson arrivals;
/// - `bursty`: twice the rate for a window, then silent for a window;
/// - `heavy_tailed`: Pareto gaps (mostly short, a few very long);
/// - `diurnal`: a quarter of the load at the start, twice it at the end;
/// - `flash_crowd`: ten times the load for the middle third;
/// - `boundary_attack`: `limit` just before a window edge and `limit` just after.
///
/// ```
/// use ratelab::RateLimitConfig;
/// use std::time::Duration;
///
/// let config = RateLimitConfig::per_second(10)?;
/// let a = sim::patterns::generate("steady", 1, Duration::from_secs(10), config, 1, 2.0).unwrap();
/// assert!((150..250).contains(&a.len())); // about 2 × 10 per second for 10 s
/// assert!(sim::patterns::generate("nope", 1, Duration::from_secs(1), config, 1, 1.0).is_err());
/// # Ok::<(), ratelab::ConfigError>(())
/// ```
pub fn generate(
    pattern: &str,
    seed: u64,
    duration: Duration,
    config: RateLimitConfig,
    keys: u32,
    load: f64,
) -> Result<Vec<Arrival>, String> {
    if !(load.is_finite() && load > 0.0 && load <= 100.0) {
        return Err("load must be between 0 and 100".to_owned());
    }
    let keys = keys.clamp(1, 10_000);
    // Mean gap between any two requests for `load` × the limit on every key.
    let rate_per_ns = config.limit() as f64 * load * f64::from(keys) / config.window_nanos() as f64;
    let gap = Duration::from_nanos((1.0 / rate_per_ns).max(1.0) as u64);
    let window = config.window();
    let d = duration;
    Ok(match pattern {
        "steady" => crate::poisson(seed, d, gap, keys),
        "bursty" => crate::on_off(seed, d, gap / 2, window, window, keys),
        "heavy_tailed" => crate::pareto(seed, d, gap, keys),
        "diurnal" => crate::diurnal(seed, d, gap * 4, gap / 2, d, keys),
        "flash_crowd" => crate::flash_crowd(seed, d, gap, keys, d / 3, d / 3, 10),
        "boundary_attack" => repeated_attack(config, d),
        other => {
            return Err(format!(
                "unknown pattern {other:?} (one of {})",
                PATTERNS.join(", ")
            ));
        }
    })
}
