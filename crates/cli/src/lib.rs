//! The `ratelab` command line: simulate traffic through every algorithm (or a cluster
//! of nodes through every coordination strategy), replay an access log to choose a
//! limit, and measure the algorithms on this machine.
//!
//! Every command prints plain tables. The logic lives here so it can be tested; the
//! binary only parses arguments.

#![forbid(unsafe_code)]

pub mod logs;

use anyhow::{Context, bail};
use ratelab::strategies::Strategy;
use ratelab::{Algorithm, Cost, FailurePolicy, ManualClock, RateLimitConfig, RateLimiter};
use sim::Arrival;
use sim::cluster::{ClusterConfig, place, run_cluster};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Parses `500ms`, `1s`, `1.5s`, `2m`, `1h` (a bare number is seconds).
///
/// ```
/// use ratelab_cli::parse_duration;
/// use std::time::Duration;
/// assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
/// assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1_500));
/// assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
/// assert_eq!(parse_duration("60").unwrap(), Duration::from_secs(60));
/// assert!(parse_duration("soon").is_err());
/// ```
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().map_err(|_| format!("not a duration: {s:?}"))?;
    let secs = match unit.trim() {
        "" | "s" => n,
        "ms" => n / 1e3,
        "us" | "µs" => n / 1e6,
        "m" | "min" => n * 60.0,
        "h" => n * 3_600.0,
        other => return Err(format!("unknown unit {other:?} in {s:?} (ms, s, m or h)")),
    };
    Duration::try_from_secs_f64(secs).map_err(|e| format!("{s:?}: {e}"))
}

/// A plain text table with right-aligned numbers.
pub fn table(head: &[&str], rows: &[Vec<String>]) -> String {
    let cols = head.len();
    let width: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .map(|r| r.get(c).map_or(0, |x| x.chars().count()))
                .chain([head[c].chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: Vec<&str>| {
        cells
            .iter()
            .enumerate()
            .map(|(c, x)| {
                let pad = width[c].saturating_sub(x.chars().count());
                if c == 0 {
                    format!("{x}{}", " ".repeat(pad))
                } else {
                    format!("{}{x}", " ".repeat(pad))
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    let mut out = line(head.to_vec());
    out.push('\n');
    out.push_str(
        &width
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  "),
    );
    out.push('\n');
    for r in rows {
        out.push_str(&line(r.iter().map(String::as_str).collect()));
        out.push('\n');
    }
    out
}

/// Settings for [`simulate`].
#[derive(Debug, Clone)]
pub struct SimulateArgs {
    /// Traffic pattern (see `sim::patterns::PATTERNS`).
    pub traffic: String,
    /// Units per window per key.
    pub limit: u64,
    /// Window.
    pub window: Duration,
    /// Offered load per key, × the limit.
    pub load: f64,
    /// Length of the run.
    pub duration: Duration,
    /// Keys.
    pub keys: u32,
    /// Nodes: 1 compares algorithms, more compares coordination strategies.
    pub nodes: usize,
    /// One-way delay to the shared store (cluster only).
    pub delay: Duration,
    /// Seed.
    pub seed: u64,
}

fn run_algorithm(
    algorithm: Algorithm,
    config: RateLimitConfig,
    arrivals: &[Arrival],
) -> sim::Trace {
    let clock = Arc::new(ManualClock::new());
    let limiter = algorithm.build(config, Arc::clone(&clock));
    sim::run(&limiter, &clock, arrivals)
}

/// Runs the traffic through every algorithm (one node) or every coordination strategy
/// (several), and returns the table.
pub fn simulate(a: &SimulateArgs) -> anyhow::Result<String> {
    let config = RateLimitConfig::new(a.limit, a.window)?;
    let arrivals = sim::patterns::generate(&a.traffic, a.seed, a.duration, config, a.keys, a.load)
        .map_err(anyhow::Error::msg)?;
    if arrivals.is_empty() {
        bail!("the run produced no requests: raise --load or --duration");
    }
    let load = if a.traffic == "boundary_attack" {
        String::new()
    } else {
        format!(" at {}×", a.load)
    };
    let mut out = format!(
        "{} requests, {} traffic{load} against a limit of {} per {:?}, {} key(s), {:?}, seed {}\n\n",
        arrivals.len(),
        a.traffic,
        a.limit,
        a.window,
        a.keys,
        a.duration,
        a.seed
    );
    if a.nodes <= 1 {
        let exact = run_algorithm(Algorithm::SlidingWindow, config, &arrivals);
        let exact_admitted = exact.steps.iter().filter(|s| s.decision.allowed).count() as i64;
        let rows: Vec<Vec<String>> = Algorithm::ALL
            .iter()
            .map(|&alg| {
                let t = run_algorithm(alg, config, &arrivals);
                let admitted = t.steps.iter().filter(|s| s.decision.allowed).count() as i64;
                let worst = t.max_admitted_in_window(a.window) as f64 / a.limit as f64;
                vec![
                    alg.name().to_owned(),
                    admitted.to_string(),
                    format!("{:+}", admitted - exact_admitted),
                    format!("{worst:.2}×"),
                ]
            })
            .collect();
        out.push_str(&table(
            &["algorithm", "admitted", "vs exact", "worst window ÷ limit"],
            &rows,
        ));
        out.push_str("\n\"vs exact\" compares with the sliding window log, which admits exactly what the rule allows.\n");
    } else {
        let placed = place(&arrivals, a.nodes, None, a.seed);
        let ideal = sim::metrics::ideal(config, &placed);
        let mut cluster = ClusterConfig::new(a.nodes);
        cluster.one_way_delay = a.delay;
        cluster.seed = a.seed;
        let rows: Vec<Vec<String>> = Strategy::defaults(config, a.nodes as u64)
            .into_iter()
            .map(|s| {
                let run = run_cluster(s, config, FailurePolicy::FailOpen, &cluster, &placed);
                let m = sim::metrics::measure(&run, &ideal, config);
                vec![
                    s.name().to_owned(),
                    format!("{:.2}%", m.over_pct + m.under_pct),
                    format!("{:.0}", m.calls_per_1k),
                    format!("{:.0} µs", m.p99_us),
                ]
            })
            .collect();
        let _ = writeln!(
            out,
            "{} nodes, {:?} one way to the shared store\n",
            a.nodes, a.delay
        );
        out.push_str(&table(
            &[
                "strategy",
                "admission error",
                "store calls / 1k",
                "p99 wait",
            ],
            &rows,
        ));
        out.push_str(
            "\nAdmission error is measured against one perfectly coordinated token bucket.\n",
        );
    }
    Ok(out)
}

/// Settings for [`replay`].
#[derive(Debug, Clone)]
pub struct ReplayArgs {
    /// The window limits are per.
    pub window: Duration,
    /// The most the exact rule may block, % of requests.
    pub target_pct: f64,
    /// A limit to try instead of the recommendation.
    pub limit: Option<u64>,
    /// Busiest callers to list.
    pub top: usize,
}

/// Replays a parsed log: recommends a limit and shows what every algorithm does with it.
pub fn replay(log: &logs::Log, a: &ReplayArgs) -> anyhow::Result<String> {
    let arrivals = &log.arrivals;
    let span = arrivals.last().map_or(0, |x| x.at.0);
    let peaks = sim::replay::key_peaks(arrivals, a.window);
    let recommended = sim::replay::recommend_limit(arrivals, a.window, a.target_pct)?;
    let limit = a.limit.unwrap_or(recommended);
    let config = RateLimitConfig::new(limit, a.window)?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} requests from {} callers over {:.1} s ({} lines skipped)\n",
        arrivals.len(),
        log.names.len(),
        span as f64 / 1e9,
        log.skipped
    );
    let _ = writeln!(
        out,
        "Busiest window per caller ({:?}): median {}, p90 {}, p99 {}, max {}",
        a.window,
        sim::replay::quantile(&peaks, 0.5),
        sim::replay::quantile(&peaks, 0.9),
        sim::replay::quantile(&peaks, 0.99),
        peaks.last().copied().unwrap_or(0)
    );
    let _ = writeln!(
        out,
        "Recommended limit: {recommended} per {:?} (the smallest at which the exact rule blocks at most {}% of requests)\n",
        a.window, a.target_pct
    );
    if a.limit.is_some() {
        let _ = writeln!(out, "Showing your limit: {limit} per {:?}\n", a.window);
    }
    let n = arrivals.len().max(1) as f64;
    let rows: Vec<Vec<String>> = Algorithm::ALL
        .iter()
        .map(|&alg| {
            let t = run_algorithm(alg, config, arrivals);
            let mut limited = std::collections::BTreeSet::new();
            for s in &t.steps {
                if !s.decision.allowed {
                    limited.insert(s.arrival.key);
                }
            }
            vec![
                alg.name().to_owned(),
                format!("{:.2}%", t.denied() as f64 * 100.0 / n),
                limited.len().to_string(),
                format!(
                    "{:.2}×",
                    t.max_admitted_in_window(a.window) as f64 / limit as f64
                ),
            ]
        })
        .collect();
    out.push_str(&table(
        &[
            "algorithm",
            "blocked",
            "callers limited",
            "worst window ÷ limit",
        ],
        &rows,
    ));
    if a.top > 0 {
        let mut counts = vec![0u64; log.names.len()];
        for x in arrivals {
            counts[x.key as usize] += 1;
        }
        let mut order: Vec<usize> = (0..counts.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(counts[i]));
        let rows: Vec<Vec<String>> = order
            .iter()
            .take(a.top)
            .map(|&i| vec![log.names[i].clone(), counts[i].to_string()])
            .collect();
        out.push('\n');
        out.push_str(&table(&["busiest callers", "requests"], &rows));
    }
    Ok(out)
}

/// Measures single-limiter speed on this machine: `threads` threads deciding for
/// `keys` keys for `duration` per algorithm. A quick look, not the published method
/// (which warms up, repeats and reports spread).
pub fn bench(duration: Duration, threads: usize, keys: usize) -> anyhow::Result<String> {
    let threads = threads.max(1);
    let keys: Vec<String> = (0..keys.max(1)).map(|k| format!("key{k}")).collect();
    let config = RateLimitConfig::per_second(1_000_000).context("limit")?;
    let mut rows = Vec::new();
    for alg in Algorithm::ALL {
        let limiter: Arc<dyn RateLimiter> =
            Arc::from(alg.build(config, ratelab::MonotonicClock::new()));
        let started = Instant::now();
        let total: u64 = std::thread::scope(|s| {
            let handles: Vec<_> = (0..threads)
                .map(|t| {
                    let limiter = Arc::clone(&limiter);
                    let keys = &keys;
                    s.spawn(move || {
                        let mut n = 0u64;
                        let mut i = t;
                        while started.elapsed() < duration {
                            for _ in 0..1_000 {
                                std::hint::black_box(
                                    limiter.check(&keys[i % keys.len()], Cost::ONE),
                                );
                                i = i.wrapping_add(7);
                            }
                            n += 1_000;
                        }
                        n
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or(0)).sum()
        });
        let secs = started.elapsed().as_secs_f64();
        rows.push(vec![
            alg.name().to_owned(),
            format!("{:.2} M/s", total as f64 / secs / 1e6),
            format!(
                "{:.0} ns",
                secs * 1e9 * threads as f64 / total.max(1) as f64
            ),
        ]);
    }
    let mut out = format!(
        "{threads} thread(s), {} key(s), {duration:?} per algorithm\n\n",
        keys.len()
    );
    out.push_str(&table(
        &["algorithm", "decisions", "per decision, per thread"],
        &rows,
    ));
    out.push_str(
        "\nA quick measurement on this machine. The published numbers come from `just results`, which warms up, repeats and reports spread.\n",
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim_args() -> SimulateArgs {
        SimulateArgs {
            traffic: "boundary_attack".into(),
            limit: 10,
            window: Duration::from_secs(1),
            load: 1.0,
            duration: Duration::from_secs(6),
            keys: 1,
            nodes: 1,
            delay: Duration::from_micros(250),
            seed: 1,
        }
    }

    #[test]
    fn the_boundary_attack_shows_the_fixed_windows_leak() {
        let out = simulate(&sim_args()).unwrap();
        let fixed = out.lines().find(|l| l.starts_with("fixed_window")).unwrap();
        assert!(fixed.contains("2.00×"), "{out}");
        let sliding = out
            .lines()
            .find(|l| l.starts_with("sliding_window"))
            .unwrap();
        assert!(sliding.contains("+0") && sliding.contains("1.00×"), "{out}");
    }

    #[test]
    fn several_nodes_compare_every_strategy() {
        let out = simulate(&SimulateArgs {
            traffic: "steady".into(),
            load: 2.0,
            nodes: 3,
            keys: 4,
            ..sim_args()
        })
        .unwrap();
        for s in [
            "central_exact",
            "local_split",
            "async_sync",
            "fixed_leasing",
            "adaptive_leasing",
        ] {
            assert!(out.contains(s), "{s} missing:\n{out}");
        }
    }

    #[test]
    fn unknown_traffic_is_an_error() {
        assert!(
            simulate(&SimulateArgs {
                traffic: "nope".into(),
                ..sim_args()
            })
            .is_err()
        );
    }

    #[test]
    fn replay_recommends_the_exact_limit() {
        let mut text = String::new();
        for i in 0..20 {
            // `a` sends one request every 100 ms; `b` sends 5 at once.
            text.push_str(&format!("{},a\n", 1_700_000_000_000u64 + i * 100));
        }
        for _ in 0..5 {
            text.push_str("1700000000000,b\n");
        }
        let log = logs::parse(&format!("ts,key\n{text}"), logs::Format::Auto, None, None).unwrap();
        let args = ReplayArgs {
            window: Duration::from_secs(1),
            target_pct: 0.0,
            limit: None,
            top: 2,
        };
        let out = replay(&log, &args).unwrap();
        assert!(out.contains("Recommended limit: 10 per 1s"), "{out}");
        assert!(out.contains("busiest callers"));
        let out = replay(
            &log,
            &ReplayArgs {
                limit: Some(3),
                ..args
            },
        )
        .unwrap();
        assert!(out.contains("Showing your limit: 3"));
    }

    #[test]
    fn bench_lists_every_algorithm() {
        let out = bench(Duration::from_millis(5), 2, 10).unwrap();
        for a in Algorithm::ALL {
            assert!(out.contains(a.name()));
        }
    }

    #[test]
    fn tables_align() {
        let t = table(&["a", "bb"], &[vec!["xyz".into(), "1".into()]]);
        assert_eq!(t, "a    bb\n---  --\nxyz   1\n");
    }
}
