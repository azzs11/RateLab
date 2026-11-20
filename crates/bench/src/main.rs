//! `cargo run -p bench --release -- <command>`
//!
//! - `local`: in-process accuracy, latency, throughput and fairness → `results/local.json`
//! - `report`: `results/*.json` → `results/RESULTS.md` and the README's results table
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
    /// Generate RESULTS.md and the README table from the result files.
    Report {
        /// Folder with the JSON files; RESULTS.md is written here.
        #[arg(long, default_value = "results")]
        dir: PathBuf,
        /// README to update between the results markers.
        #[arg(long, default_value = "README.md")]
        readme: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Local { quick, out } => run_local(quick, &out),
        Command::Report { dir, readme } => run_report(&dir, &readme),
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

fn run_report(dir: &std::path::Path, readme: &std::path::Path) -> anyhow::Result<()> {
    let inputs = Inputs {
        local: read_json(&dir.join("local.json"))?,
        memory: read_json(&dir.join("memory.json"))?,
    };
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
