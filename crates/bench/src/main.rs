//! `cargo run -p bench --release -- <command>`
//!
//! - `local`: in-process accuracy, latency, throughput and fairness → `results/local.json`
//! - `traces`: a public request log through every algorithm and strategy → `results/traces.json`
//! - `strategies`: the coordination strategies in the simulator → `results/strategies.json`
//! - `llm`: token limits on a real LLM trace (downloaded into memory) and synthetic
//!   requests, simulated and on the real layer → `results/llm.json`
//! - `overload`: concurrency limits in front of a backend with limited capacity, simulated
//!   and on the real Tower layer → `results/overload.json`
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
    /// Token limits for LLM traffic: estimate-only, reserve-max and
    /// admit-then-reconcile on the Azure LLM trace (downloaded into memory, never
    /// saved) and on synthetic requests, plus a real-time check on the server's layer.
    Llm {
        /// One seed, a short synthetic run and one real-time minute.
        #[arg(long)]
        quick: bool,
        /// Skip the real-time check.
        #[arg(long)]
        sim_only: bool,
        /// Output file.
        #[arg(long, default_value = "results/llm.json")]
        out: PathBuf,
    },
    /// No limit, static, AIMD and Gradient2 concurrency limits under overload, in the
    /// simulator and on the real Tower layer (in real time: about 5 minutes).
    Overload {
        /// Short runs and one seed, for checking the pipeline.
        #[arg(long)]
        quick: bool,
        /// Skip the real-time runs.
        #[arg(long)]
        sim_only: bool,
        /// Output file.
        #[arg(long, default_value = "results/overload.json")]
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
    /// List every number the prose templates can use, with its current value.
    Facts {
        /// Folder with the JSON files.
        #[arg(long, default_value = "results")]
        dir: PathBuf,
    },
    /// Fill the prose templates with measured numbers: docs/report/*.tmpl.md into
    /// docs/report/, and docs/story/*.tmpl.md into target/story/ (not committed).
    Story {
        /// Folder with the JSON files.
        #[arg(long, default_value = "results")]
        dir: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Local { quick, out } => run_local(quick, &out),
        Command::Strategies { quick, out } => run_strategies(quick, &out),
        Command::Traces { quick, out } => run_traces(quick, &out),
        Command::Llm {
            quick,
            sim_only,
            out,
        } => run_llm(quick, sim_only, &out),
        Command::Overload {
            quick,
            sim_only,
            out,
        } => run_overload(quick, sim_only, &out),
        Command::Report { dir, readme } => run_report(&dir, &readme),
        Command::Facts { dir } => {
            for (k, v) in bench::story::facts(&load_inputs(&dir)?) {
                println!("{k} = {v}");
            }
            Ok(())
        }
        Command::Story { dir } => run_story(&dir),
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

fn run_llm(quick: bool, sim_only: bool, out: &std::path::Path) -> anyhow::Result<()> {
    use bench::llm::{self, LlmResults, LlmSize};
    let size = if quick {
        LlmSize::quick()
    } else {
        LlmSize::full()
    };
    let started = Instant::now();
    let step = |name: &str| eprintln!("[{:>6.1} s] {name}", started.elapsed().as_secs_f64());
    let rows = llm::fetch_azure(&step)?;
    step(&format!("{} requests in the trace", rows.len()));
    let first = llm::requests(&rows, 1);
    let azure = llm::scenario(
        "azure_2023",
        "every request to two Azure LLM services (code and conversation) over one hour in November 2023, with their real input and output token counts",
        size.seeds,
        |seed| llm::requests(&rows, seed),
        &step,
    );
    let secs = size.synthetic_secs;
    let synthetic = llm::scenario(
        "synthetic",
        "Poisson arrivals at the trace's rate, with the same mean prompt and answer lengths per service but heavy-tailed (Pareto) answers",
        size.seeds,
        |seed| llm::synthetic_requests(seed, secs, &first),
        &step,
    );
    let real = if sim_only {
        None
    } else {
        Some(llm::real_check(&first, size.real_minutes, &step)?)
    };
    drop(rows);
    write_json(
        out,
        &LlmResults {
            schema: SCHEMA,
            machine: Machine::detect(),
            size,
            files: [llm::AZURE_CODE, llm::AZURE_CONV]
                .iter()
                .map(|f| (f.url.to_owned(), f.sha256.to_owned()))
                .collect(),
            homepage: llm::AZURE_HOMEPAGE.to_owned(),
            citation: llm::AZURE_CITATION.to_owned(),
            tenants: llm::TENANTS,
            window_secs: llm::WINDOW_SECS,
            target_blocked_pct: llm::TARGET_BLOCKED_PCT,
            prior_output: llm::PRIOR_OUTPUT,
            streaming_ms: (llm::FIRST_TOKEN_MS, llm::PER_TOKEN_MS),
            scenarios: vec![azure, synthetic],
            real,
        },
    )?;
    step(&format!(
        "wrote {} (the downloaded trace is gone)",
        out.display()
    ));
    Ok(())
}

fn run_overload(quick: bool, sim_only: bool, out: &std::path::Path) -> anyhow::Result<()> {
    use bench::overload::{self, OverloadResults, OverloadSize};
    let size = if quick {
        OverloadSize::quick()
    } else {
        OverloadSize::full()
    };
    let started = Instant::now();
    let step = |name: &str| eprintln!("[{:>6.1} s] {name}", started.elapsed().as_secs_f64());
    let scenarios = overload::run(&size, !sim_only, step);
    write_json(
        out,
        &OverloadResults {
            schema: SCHEMA,
            machine: Machine::detect(),
            size,
            workers: overload::WORKERS,
            service_ms: overload::SERVICE_MS,
            deadline_ms: overload::DEADLINE_MS,
            bucket_ms: overload::BUCKET_MS,
            scenarios,
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

fn load_inputs(dir: &std::path::Path) -> anyhow::Result<Inputs> {
    Ok(Inputs {
        local: read_json(&dir.join("local.json"))?,
        memory: read_json(&dir.join("memory.json"))?,
        cluster: read_json(&dir.join("cluster.json"))?,
        strategies: read_json(&dir.join("strategies.json"))?,
        traces: read_json(&dir.join("traces.json"))?,
        real_strategies: read_json(&dir.join("real_strategies.json"))?,
        overload: read_json(&dir.join("overload.json"))?,
        llm: read_json(&dir.join("llm.json"))?,
    })
}

/// Renders every `*.tmpl.md` in `from` into `to`, dropping `.tmpl`.
fn render_dir(
    facts: &bench::story::Facts,
    from: &std::path::Path,
    to: &std::path::Path,
) -> anyhow::Result<()> {
    if !from.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(to)?;
    let mut entries: Vec<_> = std::fs::read_dir(from)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".tmpl.md") else {
            continue;
        };
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let out = bench::story::render(&text, facts)
            .with_context(|| format!("render {}", path.display()))?;
        let dest = to.join(format!("{stem}.md"));
        std::fs::write(&dest, out).with_context(|| format!("write {}", dest.display()))?;
        eprintln!("wrote {}", dest.display());
    }
    Ok(())
}

fn run_story(dir: &std::path::Path) -> anyhow::Result<()> {
    let facts = bench::story::facts(&load_inputs(dir)?);
    render_dir(
        &facts,
        std::path::Path::new("docs/report"),
        std::path::Path::new("docs/report"),
    )?;
    render_dir(
        &facts,
        std::path::Path::new("docs/story"),
        std::path::Path::new("target/story"),
    )
}

fn run_report(dir: &std::path::Path, readme: &std::path::Path) -> anyhow::Result<()> {
    let inputs = load_inputs(dir)?;
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
