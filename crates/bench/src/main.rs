//! `cargo run -p bench --release -- <command>`
//!
//! - `local`: in-process accuracy, latency, throughput and fairness → `results/local.json`
//! - `traces`: a public request log through every algorithm and strategy → `results/traces.json`
//! - `strategies`: the coordination strategies in the simulator → `results/strategies.json`
//! - `report`: `results/*.json` → `results/RESULTS.md` and the README's results table
//! - `machine`: print this machine's description
//!
//! Memory per key is a separate binary (`--bin memory`), and the 3-node run is the
//! `loadgen` binary inside Docker Compose. `just results` runs all of them.

use anyhow::Context;
use bench::local::{self, SuiteSize};
use bench::machine::Machine;
use bench::report::{self, Inputs};
use bench::results::{LocalResults, SCHEMA, read_json, write_json};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
#[command(about = "Reproducible rate limiter measurements")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the in-process suite.
    Local {
        /// Small sizes for CI; the numbers are not meaningful.
        #[arg(long)]
        quick: bool,
        /// Output file.
        #[arg(long, default_value = "results/local.json")]
        out: PathBuf,
    },
    /// Compare the coordination strategies in the multi-node simulator.
    Strategies {
        /// Small sizes for CI; the numbers are not meaningful.
        #[arg(long)]
        quick: bool,
        /// Output file.
        #[arg(long, default_value = "results/strategies.json")]
        out: PathBuf,
    },
    /// Replay a public request log (downloaded into memory, never saved) through every
    /// algorithm and strategy.
    Traces {
        /// Only two days of the smaller file, for checking the pipeline.
        #[arg(long)]
        quick: bool,
        /// Output file.
        #[arg(long, default_value = "results/traces.json")]
        out: PathBuf,
    },
    /// Generate RESULTS.md and the README table from the result files.
    Report {
        /// Folder with the JSON files; RESULTS.md is written here.
        #[arg(long, default_value = "results")]
        dir: PathBuf,
        /// README to update between the results markers.
        #[arg(long, default_value = "README.md")]
        readme: PathBuf,
    },
    /// Print this machine's description (used to label the Docker run).
    Machine,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Local { quick, out } => run_local(quick, &out),
        Command::Strategies { quick, out } => run_strategies(quick, &out),
        Command::Traces { quick, out } => run_traces(quick, &out),
        Command::Report { dir, readme } => run_report(&dir, &readme),
        Command::Machine => {
            let m = Machine::detect();
            println!("{} · commit `{}`", m.one_line(), m.git_commit);
            Ok(())
        }
    }
}

fn run_local(quick: bool, out: &std::path::Path) -> anyhow::Result<()> {
    if cfg!(debug_assertions) {
        eprintln!(
            "warning: debug build; latency and throughput are not representative (use --release)"
        );
    }
    let size = if quick {
        SuiteSize::quick()
    } else {
        SuiteSize::full()
    };
    let machine = Machine::detect();
    eprintln!("machine: {}", machine.one_line());
    let started = Instant::now();
    let step = |name: &str| eprintln!("[{:>6.1} s] {name}", started.elapsed().as_secs_f64());

    step("accuracy");
    let accuracy = local::accuracy(&size);
    step("fairness");
    let fairness = local::fairness(&size);
    step("latency");
    let timer = local::timer_info();
    let latency = local::latency(&size);
    let throughput = local::throughput(&size, step);

    write_json(
        out,
        &LocalResults {
            schema: SCHEMA,
            machine,
            size,
            timer,
            accuracy,
            latency,
            throughput,
            fairness,
        },
    )?;
    step(&format!("wrote {}", out.display()));
    Ok(())
}

fn run_strategies(quick: bool, out: &std::path::Path) -> anyhow::Result<()> {
    use bench::strategies::{self, StrategySize};
    let size = if quick {
        StrategySize::quick()
    } else {
        StrategySize::full()
    };
    let started = Instant::now();
    let step = |name: &str| eprintln!("[{:>6.1} s] {name}", started.elapsed().as_secs_f64());
    let matrix = strategies::matrix(&size, step);
    step("skew");
    let skew = strategies::skew(&size);
    step("outage");
    let outage = strategies::outage(&size);
    step("convergence");
    let convergence = strategies::convergence(&size);
    write_json(
        out,
        &bench::results::StrategyResults {
            schema: SCHEMA,
            machine: Machine::detect(),
            size,
            network: strategies::NETWORK,
            limit: local::ACCURACY_LIMIT,
            keys: local::ACCURACY_KEYS,
            matrix,
            skew,
            outage,
            convergence,
        },
    )?;
    step(&format!("wrote {}", out.display()));
    Ok(())
}

fn run_traces(quick: bool, out: &std::path::Path) -> anyhow::Result<()> {
    use bench::real_traffic::{self, TARGET_BLOCKED_PCT, TraceResults, WINDOW_SECS};
    use bench::traces::{self, NASA_KSC_1995, TraceInfo};
    let started = Instant::now();
    let step = |name: &str| eprintln!("[{:>6.1} s] {name}", started.elapsed().as_secs_f64());
    let dataset = NASA_KSC_1995;
    let files: Vec<_> = if quick {
        dataset.files[1..].to_vec()
    } else {
        dataset.files.to_vec()
    };

    // Download into memory only; nothing is written to disk.
    let mut blobs = Vec::new();
    for f in &files {
        step(&format!("downloading {}", f.url));
        blobs.push(traces::fetch(f)?);
    }
    step("parsing");
    let requests = traces::parse_clf_gz(&blobs)?;
    drop(blobs);
    let mut arrivals = traces::arrivals(&requests);
    if quick {
        arrivals.retain(|a| a.at.0 < 2 * 86_400 * 1_000_000_000);
    }
    let span_secs = arrivals.last().map_or(0, |a| a.at.0 / 1_000_000_000);
    let info = TraceInfo {
        name: dataset.name.to_owned(),
        homepage: dataset.homepage.to_owned(),
        files: files
            .iter()
            .map(|f| (f.url.to_owned(), f.sha256.to_owned()))
            .collect(),
        requests: arrivals.len() as u64,
        clients: requests.clients,
        skipped: requests.skipped,
        span_secs,
        resolution_secs: 1,
    };
    drop(requests);
    step(&format!(
        "{} requests from {} clients over {} days",
        info.requests,
        info.clients,
        span_secs / 86_400
    ));
    let (limit, algorithms, strategies) = real_traffic::replay(&arrivals, &[3, 8], step);
    write_json(
        out,
        &TraceResults {
            schema: SCHEMA,
            machine: Machine::detect(),
            trace: info,
            window_secs: WINDOW_SECS,
            limit,
            target_blocked_pct: TARGET_BLOCKED_PCT,
            algorithms,
            strategies,
        },
    )?;
    step(&format!(
        "wrote {} (the downloaded log is gone)",
        out.display()
    ));
    Ok(())
}

fn run_report(dir: &std::path::Path, readme: &std::path::Path) -> anyhow::Result<()> {
    let inputs = Inputs {
        local: read_json(&dir.join("local.json"))?,
        memory: read_json(&dir.join("memory.json"))?,
        cluster: read_json(&dir.join("cluster.json"))?,
        strategies: read_json(&dir.join("strategies.json"))?,
        traces: read_json(&dir.join("traces.json"))?,
        real_strategies: read_json(&dir.join("real_strategies.json"))?,
    };
    for (name, svg) in report::charts(&inputs) {
        let path = dir.join(&name);
        std::fs::write(&path, svg).with_context(|| format!("write {}", path.display()))?;
        eprintln!("wrote {}", path.display());
    }
    let results_path = dir.join("RESULTS.md");
    std::fs::write(&results_path, report::results_md(&inputs))
        .with_context(|| format!("write {}", results_path.display()))?;
    eprintln!("wrote {}", results_path.display());

    let text =
        std::fs::read_to_string(readme).with_context(|| format!("read {}", readme.display()))?;
    let updated = report::splice_readme(&text, &report::readme_block(&inputs))?;
    std::fs::write(readme, updated).with_context(|| format!("write {}", readme.display()))?;
    eprintln!("updated {}", readme.display());
    Ok(())
}
