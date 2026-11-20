//! `cargo run -p bench --release --bin memory -- [--quick] [--out FILE]`
//!
//! Heap bytes per key for every algorithm, counted by a wrapping global allocator.
//! Separate from the main bench so the counting does not touch its latency numbers.

use bench::machine::Machine;
use bench::memory::measure;
use bench::results::{MemoryResults, write_json};
use cap::Cap;
use std::alloc::System;
use std::path::PathBuf;

#[global_allocator]
static ALLOCATOR: Cap<System> = Cap::new(System, usize::MAX);

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let quick = args.iter().any(|a| a == "--quick");
    let out = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .map_or_else(|| PathBuf::from("results/memory.json"), PathBuf::from);

    let (keys, limit) = if quick { (1_000, 10) } else { (100_000, 100) };
    let machine = Machine::detect();
    eprintln!("memory: {keys} keys, limit {limit}");
    let rows = measure(keys, limit, || ALLOCATOR.allocated());
    write_json(&out, &MemoryResults { machine, rows })?;
    eprintln!("wrote {}", out.display());
    Ok(())
}
