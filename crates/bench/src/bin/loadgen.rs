//! Load generator for the multi-node run (`deploy/docker-compose.yml`).
//!
//! Sends seeded open-loop traffic to every node, records every request, reads Redis's
//! own counters for cost, injects a Redis outage, and writes `results/cluster.json`.
//! The limit is read from the servers' `RateLimit-Policy` header, so the analysis
//! always uses the limit the servers actually enforce.

use anyhow::{Context, bail};
use bench::cluster::{
    self, BreakerSettings, ClusterResults, ResilienceRow, Route, Sample, ScenarioRow, Source,
    ThroughputRow,
};
use bench::machine::Machine;
use bench::results::{SCHEMA, write_json};
use bench::stats::Summary;
use clap::Parser;
use ratelab::RateLimitConfig;
use sim::SplitMix64;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio::time::Instant;

#[derive(Parser)]
#[command(about = "Multi-node load generator")]
struct Args {
    /// Comma-separated base URLs of the server nodes.
    #[arg(
        long,
        env = "DRL_TARGETS",
        default_value = "http://127.0.0.1:3001",
        value_delimiter = ','
    )]
    targets: Vec<String>,
    /// Redis the nodes share (for INFO counters and the outage).
    #[arg(long, env = "DRL_REDIS_URL", default_value = "redis://127.0.0.1:6379/")]
    redis_url: String,
    /// Small sizes for CI; the numbers are not meaningful.
    #[arg(long, env = "DRL_QUICK")]
    quick: bool,
    /// Output file.
    #[arg(long, env = "DRL_OUT", default_value = "results/cluster.json")]
    out: PathBuf,
    /// Description of the host, recorded with the results.
    #[arg(long, env = "DRL_HOST")]
    host: Option<String>,
    /// The nodes' breaker threshold (recorded, not applied).
    #[arg(long, env = "DRL_BREAKER_FAILURES")]
    breaker_failures: Option<u32>,
    /// The nodes' breaker open time in ms (recorded, not applied).
    #[arg(long, env = "DRL_BREAKER_OPEN_MS")]
    breaker_open_ms: Option<u64>,
    /// The nodes' Redis timeout in ms (recorded, not applied).
    #[arg(long, env = "DRL_REDIS_TIMEOUT_MS")]
    redis_timeout_ms: Option<u64>,
    /// `cluster` (the Redis token bucket: accuracy, throughput, outage) or `strategies`
    /// (every coordination strategy on the real cluster, against the simulator).
    #[arg(long, env = "DRL_MODE", default_value = "cluster")]
    mode: String,
}

struct Size {
    name: &'static str,
    run_secs: u64,
    seeds: u64,
    keys: u32,
    load_factor: f64,
    throughput_secs: u64,
    throughput_runs: usize,
    concurrency: usize,
    outage: (u64, u64, u64),
}

const FULL: Size = Size {
    name: "full",
    run_secs: 20,
    seeds: 5,
    keys: 30,
    load_factor: 2.0,
    throughput_secs: 10,
    throughput_runs: 5,
    concurrency: 64,
    outage: (5, 10, 15),
};

const QUICK: Size = Size {
    name: "quick",
    run_secs: 3,
    seeds: 1,
    keys: 5,
    load_factor: 2.0,
    throughput_secs: 2,
    throughput_runs: 1,
    concurrency: 8,
    outage: (1, 2, 3),
};

/// Share of traffic sent to node 0 in the skewed scenario, in %.
const SKEWED_NODE0_PCT: u64 = 80;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let size = if args.quick { &QUICK } else { &FULL };
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(256)
        .build()?;
    let targets = Arc::new(args.targets.clone());

    wait_until_ready(&client, &targets).await?;
    let config = read_policy(&client, &targets[0]).await?;
    eprintln!(
        "{} nodes, limit {} per {:?}, size {}",
        targets.len(),
        config.limit(),
        config.window(),
        size.name
    );
    let mut redis = redis::Client::open(args.redis_url.as_str())?
        .get_multiplexed_async_connection()
        .await
        .context("connect to Redis")?;

    if args.mode == "strategies" {
        return strategies_mode(&args, &client, &targets, &mut redis, size, config).await;
    }

    let mut scenarios = Vec::new();
    for (name, node0_pct) in [("uniform", None), ("skewed", Some(SKEWED_NODE0_PCT))] {
        let mut runs = Vec::new();
        for seed in 1..=size.seeds {
            eprintln!("scenario {name} seed {seed}");
            let plan = plan(seed, size, config, targets.len(), node0_pct, false);
            let before = redis_counters(&mut redis).await?;
            let samples = send(&client, &targets, &plan, &format!("{name}-{seed}-")).await;
            let after = redis_counters(&mut redis).await?;
            runs.push((samples, before, after));
        }
        scenarios.push(summarise(
            name,
            node0_pct,
            size,
            config,
            targets.len(),
            &runs,
        ));
    }

    eprintln!("throughput");
    let throughput = Some(throughput(&client, &targets, size).await);

    eprintln!("outage");
    let (resilience, recovery_ms) = outage(&client, &targets, &mut redis, size, config).await?;

    let results = ClusterResults {
        schema: SCHEMA,
        machine: Machine::detect(),
        host: args.host.filter(|h| !h.is_empty()),
        nodes: targets.len(),
        limit: config.limit(),
        window_secs: config.window().as_secs(),
        size: size.name.to_owned(),
        run_secs: size.run_secs,
        seeds: size.seeds,
        keys: size.keys,
        load_factor: size.load_factor,
        scenarios,
        throughput,
        breaker: match (
            args.breaker_failures,
            args.breaker_open_ms,
            args.redis_timeout_ms,
        ) {
            (Some(failures), Some(open_ms), Some(redis_timeout_ms)) => Some(BreakerSettings {
                failures,
                open_ms,
                redis_timeout_ms,
            }),
            _ => None,
        },
        outage_secs: (size.outage.0 as f64, size.outage.1 as f64),
        resilience,
        recovery_ms,
    };
    write_json(&args.out, &results)?;
    eprintln!("wrote {}", args.out.display());
    Ok(())
}

async fn wait_until_ready(client: &reqwest::Client, targets: &[String]) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    for t in targets {
        loop {
            let ok = client
                .get(format!("{t}/health"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
            if ok {
                break;
            }
            if Instant::now() > deadline {
                bail!("{t} did not become healthy within 60 s");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    Ok(())
}

/// Reads `q` and `w` from the `RateLimit-Policy` header.
async fn read_policy(client: &reqwest::Client, target: &str) -> anyhow::Result<RateLimitConfig> {
    let r = client
        .get(format!("{target}/api/hello"))
        .header("x-api-key", "loadgen-probe")
        .send()
        .await?;
    let header = r
        .headers()
        .get("ratelimit-policy")
        .and_then(|v| v.to_str().ok())
        .context("no RateLimit-Policy header")?
        .to_owned();
    let param = |name: &str| -> anyhow::Result<u64> {
        header
            .split(';')
            .find_map(|p| p.strip_prefix(&format!("{name}=")))
            .with_context(|| format!("no {name} in {header:?}"))?
            .parse()
            .with_context(|| format!("bad {name} in {header:?}"))
    };
    Ok(RateLimitConfig::new(
        param("q")?,
        Duration::from_secs(param("w")?),
    )?)
}

/// One request to send.
#[derive(Clone, Copy)]
struct Planned {
    at_ns: u64,
    key: u32,
    node: usize,
    route: Route,
}

/// Poisson arrivals at `load_factor` × the limit per key; nodes uniform, or node 0
/// getting `node0_pct` % and the rest split evenly.
fn plan(
    seed: u64,
    size: &Size,
    config: RateLimitConfig,
    nodes: usize,
    node0_pct: Option<u64>,
    both_routes: bool,
) -> Vec<Planned> {
    let per_key_rate = config.limit() as f64 / config.window().as_secs_f64() * size.load_factor;
    let gap = Duration::from_secs_f64(1.0 / (per_key_rate * f64::from(size.keys)));
    let arrivals = sim::poisson(seed, Duration::from_secs(size.run_secs), gap, size.keys);
    let mut rng = SplitMix64::new(seed ^ 0x5EED);
    arrivals
        .into_iter()
        .map(|a| {
            let node = match node0_pct {
                Some(p) if nodes > 1 => {
                    if rng.below(100) < p {
                        0
                    } else {
                        1 + rng.below(nodes as u64 - 1) as usize
                    }
                }
                _ => rng.below(nodes as u64) as usize,
            };
            let route = if both_routes && rng.below(2) == 1 {
                Route::Orders
            } else {
                Route::Public
            };
            Planned {
                at_ns: a.at.0,
                key: a.key,
                node,
                route,
            }
        })
        .collect()
}

/// Sends `plan` open-loop: each request leaves at its planned time, whether or not
/// earlier ones have answered. Records the actual send time.
async fn send(
    client: &reqwest::Client,
    targets: &Arc<Vec<String>>,
    plan: &[Planned],
    key_prefix: &str,
) -> Vec<Sample> {
    let start = Instant::now() + Duration::from_millis(50);
    let mut tasks = JoinSet::new();
    for p in plan.iter().copied() {
        tokio::time::sleep_until(start + Duration::from_nanos(p.at_ns)).await;
        let client = client.clone();
        let targets = Arc::clone(targets);
        let key = format!("{key_prefix}{}", p.key);
        tasks.spawn(async move {
            let sent = Instant::now();
            let base = &targets[p.node];
            let request = match p.route {
                Route::Public => client.get(format!("{base}/api/hello")),
                Route::Orders => client.post(format!("{base}/api/orders")),
            };
            let response = request.header("x-api-key", key).send().await;
            let latency_us = sent.elapsed().as_micros() as u64;
            let (status, source) = match &response {
                Ok(r) => (
                    r.status().as_u16(),
                    Source::from_header(
                        r.headers()
                            .get("x-drl-source")
                            .and_then(|v| v.to_str().ok()),
                    ),
                ),
                Err(_) => (0, Source::NoResponse),
            };
            Sample {
                at_ns: sent.duration_since(start).as_nanos() as u64,
                key: p.key,
                node: p.node,
                route: p.route,
                status,
                latency_us,
                source,
            }
        });
    }
    let mut samples: Vec<Sample> = tasks.join_all().await;
    samples.sort_by_key(|s| s.at_ns);
    samples
}

/// Redis's own counters, read with `INFO stats` and `INFO commandstats`.
#[derive(Clone, Copy)]
struct Counters {
    /// Script calls from clients: EVALSHA, EVAL and SCRIPT (LOAD) round trips.
    calls: u64,
    /// Every command executed, including the ones inside the Lua script.
    commands: u64,
    /// Network bytes in and out.
    bytes: u64,
    /// Bytes of the two INFO replies, to subtract from the next reading.
    info_bytes: u64,
}

async fn redis_counters(redis: &mut redis::aio::MultiplexedConnection) -> anyhow::Result<Counters> {
    let stats: String = redis::cmd("INFO").arg("stats").query_async(redis).await?;
    let commandstats: String = redis::cmd("INFO")
        .arg("commandstats")
        .query_async(redis)
        .await?;
    let field = |name: &str| -> u64 {
        stats
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name}:")))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    };
    let calls = commandstats
        .lines()
        .filter(|l| {
            ["cmdstat_evalsha", "cmdstat_eval", "cmdstat_script"]
                .iter()
                .any(|p| l.starts_with(p))
        })
        .filter_map(|l| {
            l.split("calls=")
                .nth(1)?
                .split(',')
                .next()?
                .parse::<u64>()
                .ok()
        })
        .sum();
    Ok(Counters {
        calls,
        commands: field("total_commands_processed"),
        bytes: field("total_net_input_bytes") + field("total_net_output_bytes"),
        info_bytes: (stats.len() + commandstats.len()) as u64,
    })
}

fn summarise(
    name: &str,
    node0_pct: Option<u64>,
    size: &Size,
    config: RateLimitConfig,
    nodes: usize,
    runs: &[(Vec<Sample>, Counters, Counters)],
) -> ScenarioRow {
    let mut cols: Vec<Vec<f64>> = vec![Vec::new(); 14];
    for (samples, before, after) in runs {
        let acc = cluster::accuracy(config, samples);
        let decisions = samples.len().max(1) as f64;
        let calls = after.calls.saturating_sub(before.calls) as f64;
        // The two INFO commands of the first reading are counted in the second one.
        let commands = after.commands.saturating_sub(before.commands + 2) as f64;
        let bytes = after.bytes.saturating_sub(before.bytes + before.info_bytes) as f64;
        let (p50, p99, p999) = cluster::latency_ms(samples);
        let no_response = samples
            .iter()
            .filter(|s| s.source == Source::NoResponse)
            .count() as f64;
        let values = [
            samples.len() as f64,
            samples.len() as f64 / size.run_secs as f64,
            acc.over_vs_oracle_pct,
            acc.under_vs_oracle_pct,
            acc.over_vs_single_pct,
            acc.under_vs_single_pct,
            acc.worst_window_ratio,
            calls * 1_000.0 / decisions,
            commands * 1_000.0 / decisions,
            bytes / decisions,
            p50,
            p99,
            p999,
            no_response * 100.0 / decisions,
        ];
        for (col, v) in cols.iter_mut().zip(values) {
            col.push(v);
        }
    }
    let s = |i: usize| Summary::of(&cols[i]);
    ScenarioRow {
        scenario: name.to_owned(),
        node0_share_pct: node0_pct.map_or(100.0 / nodes as f64, |p| p as f64),
        requests: s(0),
        sent_per_sec: s(1),
        over_vs_oracle_pct: s(2),
        under_vs_oracle_pct: s(3),
        over_vs_single_pct: s(4),
        under_vs_single_pct: s(5),
        worst_window_ratio: s(6),
        redis_calls_per_1k: s(7),
        redis_commands_per_1k: s(8),
        redis_bytes_per_decision: s(9),
        p50_ms: s(10),
        p99_ms: s(11),
        p999_ms: s(12),
        no_response_pct: s(13),
    }
}

/// Closed loop: `concurrency` workers send back-to-back requests over many keys.
async fn throughput(
    client: &reqwest::Client,
    targets: &Arc<Vec<String>>,
    size: &Size,
) -> ThroughputRow {
    let mut rps = Vec::new();
    let mut p50 = Vec::new();
    let mut p99 = Vec::new();
    for run in 0..=size.throughput_runs {
        let end = Instant::now() + Duration::from_secs(size.throughput_secs);
        let mut tasks = JoinSet::new();
        for w in 0..size.concurrency {
            let client = client.clone();
            let targets = Arc::clone(targets);
            tasks.spawn(async move {
                let mut rng = SplitMix64::new((run as u64) << 32 | w as u64);
                let mut latencies = Vec::new();
                while Instant::now() < end {
                    let node = rng.below(targets.len() as u64) as usize;
                    let key = format!("tp-{run}-{}", rng.below(1_000_000));
                    let sent = Instant::now();
                    let ok = client
                        .get(format!("{}/api/hello", targets[node]))
                        .header("x-api-key", key)
                        .send()
                        .await
                        .is_ok();
                    if ok {
                        latencies.push(sent.elapsed().as_micros() as u64);
                    }
                }
                latencies
            });
        }
        let mut all: Vec<u64> = tasks.join_all().await.into_iter().flatten().collect();
        if run == 0 {
            continue; // warm-up
        }
        all.sort_unstable();
        rps.push(all.len() as f64 / size.throughput_secs as f64);
        p50.push(bench::stats::percentile(&all, 50.0) as f64 / 1_000.0);
        p99.push(bench::stats::percentile(&all, 99.0) as f64 / 1_000.0);
    }
    ThroughputRow {
        concurrency: size.concurrency,
        secs: size.throughput_secs,
        requests_per_sec: Summary::of(&rps),
        p50_ms: Summary::of(&p50),
        p99_ms: Summary::of(&p99),
    }
}

/// Steady traffic on both routes; Redis stops running scripts between the outage
/// marks (`CLIENT PAUSE ... WRITE`), then recovers.
async fn outage(
    client: &reqwest::Client,
    targets: &Arc<Vec<String>>,
    redis: &mut redis::aio::MultiplexedConnection,
    size: &Size,
    config: RateLimitConfig,
) -> anyhow::Result<(Vec<ResilienceRow>, Option<f64>)> {
    let (start_s, end_s, total_s) = size.outage;
    let outage_size = Size {
        run_secs: total_s,
        ..*size
    };
    let plan = plan(99, &outage_size, config, targets.len(), None, true);
    let pause_ms = (end_s - start_s) * 1_000;
    let mut admin = redis.clone();
    // Same 50 ms lead as `send`, so the marks line up with the samples' clock.
    let begin = Instant::now() + Duration::from_millis(50);
    let pauser = tokio::spawn(async move {
        tokio::time::sleep_until(begin + Duration::from_secs(start_s)).await;
        redis::cmd("CLIENT")
            .arg("PAUSE")
            .arg(pause_ms)
            .arg("WRITE")
            .query_async::<()>(&mut admin)
            .await
    });
    let samples = send(client, targets, &plan, "outage-").await;
    pauser.await??;
    let end_ns = end_s * 1_000_000_000;
    Ok((
        cluster::resilience(config, &samples, start_s * 1_000_000_000, end_ns),
        cluster::recovery_ms(&samples, targets.len(), end_ns),
    ))
}

/// Every coordination strategy on the real cluster: the nodes switch strategy through
/// their benchmark-only admin route, Redis is emptied, and the same traffic is sent.
async fn strategies_mode(
    args: &Args,
    client: &reqwest::Client,
    targets: &Arc<Vec<String>>,
    redis: &mut redis::aio::MultiplexedConnection,
    size: &Size,
    config: RateLimitConfig,
) -> anyhow::Result<()> {
    use bench::real_strategies::{self, RealStrategyResults};
    use ratelab::strategies::Strategy;
    let seeds = if args.quick { 1 } else { 3 };
    let nodes = targets.len();
    let mut rows = Vec::new();
    for strategy in Strategy::defaults(config, nodes as u64) {
        let name = strategy.name();
        for t in targets.iter() {
            client
                .post(format!("{t}/admin/strategy/{name}"))
                .send()
                .await?
                .error_for_status()
                .with_context(|| {
                    format!("switch {t} to {name} (is DRL_ADMIN=1 set on the nodes?)")
                })?;
        }
        let _: () = redis::cmd("FLUSHALL").query_async(redis).await?;
        for (scenario, node0_pct) in [("uniform", None), ("skewed", Some(SKEWED_NODE0_PCT))] {
            let mut runs = Vec::new();
            for seed in 1..=seeds {
                eprintln!("{name} {scenario} seed {seed}");
                let plan = plan(seed, size, config, nodes, node0_pct, false);
                let before = redis_counters(redis).await?;
                let samples = send(
                    client,
                    targets,
                    &plan,
                    &format!("{name}-{scenario}-{seed}-"),
                )
                .await;
                let after = redis_counters(redis).await?;
                let calls = after.calls.saturating_sub(before.calls);
                runs.push(real_strategies::compare(
                    strategy, config, nodes, &samples, calls,
                ));
            }
            let node0 = node0_pct.map_or(100.0 / nodes as f64, |p| p as f64);
            rows.push(real_strategies::row(name, scenario, node0, &runs));
        }
    }
    let results = RealStrategyResults {
        schema: SCHEMA,
        machine: Machine::detect(),
        host: args.host.clone().filter(|h| !h.is_empty()),
        nodes,
        limit: config.limit(),
        window_secs: config.window().as_secs(),
        keys: size.keys,
        load_factor: size.load_factor,
        run_secs: size.run_secs,
        seeds,
        rows,
    };
    write_json(&args.out, &results)?;
    eprintln!("wrote {}", args.out.display());
    Ok(())
}
