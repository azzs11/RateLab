//! `ratelab`: rate limiting experiments from the command line.
//!
//! ```text
//! ratelab simulate --traffic bursty --limit 10 --window 1s --load 2
//! ratelab simulate --nodes 3 --traffic steady --load 2       # coordination strategies
//! ratelab replay access.log --window 1m --target 1           # choose a limit from a log
//! ratelab bench --threads 4                                  # quick speed check
//! ```

use clap::{Parser, Subcommand};
use ratelab_cli::logs::{self, Format};
use ratelab_cli::{ReplayArgs, SimulateArgs, bench, parse_duration, replay, simulate};
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "ratelab",
    version,
    about = "Rate limiting experiments: simulate, replay a log, benchmark"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send generated traffic through every algorithm, or through a cluster with every
    /// coordination strategy (--nodes 2 or more).
    Simulate {
        /// steady, bursty, heavy_tailed, diurnal, flash_crowd or boundary_attack.
        #[arg(long, default_value = "bursty")]
        traffic: String,
        /// Requests allowed per window, per key.
        #[arg(long, default_value_t = 10)]
        limit: u64,
        /// The window (500ms, 1s, 1m, ...).
        #[arg(long, default_value = "1s", value_parser = parse_duration)]
        window: Duration,
        /// Offered load per key, as a multiple of the limit.
        #[arg(long, default_value_t = 2.0)]
        load: f64,
        /// Length of the run.
        #[arg(long, default_value = "20s", value_parser = parse_duration)]
        duration: Duration,
        /// Keys (callers).
        #[arg(long, default_value_t = 1)]
        keys: u32,
        /// Nodes sharing the limit; 2 or more compares coordination strategies.
        #[arg(long, default_value_t = 1)]
        nodes: usize,
        /// One-way delay between a node and the shared store.
        #[arg(long, default_value = "250us", value_parser = parse_duration)]
        delay: Duration,
        /// Random seed.
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Replay an access log (nginx/Apache, CSV or JSON Lines; `-` reads stdin) and
    /// recommend a limit per caller.
    Replay {
        /// The log file, or `-` for stdin. Files ending in .gz are decompressed.
        file: PathBuf,
        /// The window limits are counted in.
        #[arg(long, default_value = "1m", value_parser = parse_duration)]
        window: Duration,
        /// The most the exact rule may block, % of requests.
        #[arg(long, default_value_t = 1.0)]
        target: f64,
        /// Try this limit instead of the recommendation.
        #[arg(long)]
        limit: Option<u64>,
        /// Log format.
        #[arg(long, value_enum, default_value = "auto")]
        format: Format,
        /// CSV column or JSON field with the time.
        #[arg(long)]
        time_column: Option<String>,
        /// CSV column or JSON field with the caller.
        #[arg(long)]
        key_column: Option<String>,
        /// Busiest callers to list.
        #[arg(long, default_value_t = 5)]
        top: usize,
    },
    /// Measure every algorithm's decisions per second on this machine (a quick look).
    Bench {
        /// Time per algorithm.
        #[arg(long, default_value = "1s", value_parser = parse_duration)]
        duration: Duration,
        /// Threads sharing one limiter.
        #[arg(long, default_value_t = 1)]
        threads: usize,
        /// Distinct keys.
        #[arg(long, default_value_t = 1_000)]
        keys: usize,
    },
}

fn read_log(file: &PathBuf) -> anyhow::Result<String> {
    let mut bytes = Vec::new();
    if file.as_os_str() == "-" {
        std::io::stdin().read_to_end(&mut bytes)?;
    } else {
        bytes = std::fs::read(file).map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    }
    if file.extension().is_some_and(|e| e == "gz") {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice()).read_to_end(&mut out)?;
        bytes = out;
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn main() -> anyhow::Result<()> {
    let out = match Cli::parse().command {
        Command::Simulate {
            traffic,
            limit,
            window,
            load,
            duration,
            keys,
            nodes,
            delay,
            seed,
        } => simulate(&SimulateArgs {
            traffic,
            limit,
            window,
            load,
            duration,
            keys,
            nodes,
            delay,
            seed,
        })?,
        Command::Replay {
            file,
            window,
            target,
            limit,
            format,
            time_column,
            key_column,
            top,
        } => {
            let text = read_log(&file)?;
            let log = logs::parse(&text, format, time_column.as_deref(), key_column.as_deref())?;
            replay(
                &log,
                &ReplayArgs {
                    window,
                    target_pct: target,
                    limit,
                    top,
                },
            )?
        }
        Command::Bench {
            duration,
            threads,
            keys,
        } => bench(duration, threads, keys)?,
    };
    print!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }
}
