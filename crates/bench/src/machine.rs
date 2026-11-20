//! The machine a result was measured on, recorded automatically with every result file.
//!
//! Read from the OS (`sysctl` and `sw_vers` on macOS; `/proc` and `/etc/os-release` on
//! Linux). Anything that cannot be read is recorded as `"unknown"`, never guessed.

use serde::{Deserialize, Serialize};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Hardware and software a measurement ran on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    /// CPU model name.
    pub cpu: String,
    /// Logical CPUs available to this process.
    pub logical_cpus: usize,
    /// Total memory in bytes (0 if unknown).
    pub memory_bytes: u64,
    /// Operating system and version.
    pub os: String,
    /// CPU architecture.
    pub arch: String,
    /// rustc that built the benchmark.
    pub rustc: String,
    /// Build profile (`release` or `debug`).
    pub profile: String,
    /// Git commit of the measured code, with `-dirty` if it had local changes.
    pub git_commit: String,
    /// When the measurement started, UTC, RFC 3339.
    pub measured_at: String,
}

const UNKNOWN: &str = "unknown";

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn cpu() -> Option<String> {
    if cfg!(target_os = "macos") {
        return run("sysctl", &["-n", "machdep.cpu.brand_string"]);
    }
    let info = read("/proc/cpuinfo")?;
    info.lines()
        .find(|l| l.starts_with("model name") || l.starts_with("Model"))
        .and_then(|l| l.split(':').nth(1))
        .map(|s| s.trim().to_owned())
}

fn memory_bytes() -> Option<u64> {
    if cfg!(target_os = "macos") {
        return run("sysctl", &["-n", "hw.memsize"])?.parse().ok();
    }
    let info = read("/proc/meminfo")?;
    let kib: u64 = info
        .lines()
        .find(|l| l.starts_with("MemTotal:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kib * 1024)
}

fn os() -> Option<String> {
    if cfg!(target_os = "macos") {
        let name = run("sw_vers", &["-productName"])?;
        let version = run("sw_vers", &["-productVersion"])?;
        return Some(format!("{name} {version}"));
    }
    let release = read("/etc/os-release")?;
    let pretty = release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))?
        .trim_matches('"')
        .to_owned();
    let kernel = run("uname", &["-r"]).unwrap_or_else(|| UNKNOWN.to_owned());
    Some(format!("{pretty} (Linux {kernel})"))
}

fn git_commit() -> Option<String> {
    if let Ok(commit) = std::env::var("DRL_GIT_COMMIT")
        && !commit.is_empty()
    {
        return Some(commit);
    }
    let commit = run("git", &["rev-parse", "--short=12", "HEAD"])?;
    // Generated files (results and the README table) do not count as code changes.
    let dirty = run(
        "git",
        &[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
            ".",
            ":!results",
            ":!README.md",
        ],
    )
    .is_some_and(|s| !s.is_empty());
    Some(if dirty {
        format!("{commit}-dirty")
    } else {
        commit
    })
}

/// RFC 3339 UTC timestamp for seconds since the Unix epoch.
pub fn rfc3339(secs: u64) -> String {
    // Civil-from-days (Howard Hinnant's algorithm).
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

impl Machine {
    /// Reads the current machine.
    pub fn detect() -> Self {
        let or_unknown = |v: Option<String>| v.unwrap_or_else(|| UNKNOWN.to_owned());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Self {
            cpu: or_unknown(cpu()),
            logical_cpus: std::thread::available_parallelism().map_or(0, |n| n.get()),
            memory_bytes: memory_bytes().unwrap_or(0),
            os: or_unknown(os()),
            arch: std::env::consts::ARCH.to_owned(),
            rustc: env!("DRL_RUSTC_VERSION").to_owned(),
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_owned(),
            git_commit: or_unknown(git_commit()),
            measured_at: rfc3339(now),
        }
    }

    /// One line for report headers.
    pub fn one_line(&self) -> String {
        format!(
            "{}, {} logical CPUs, {:.0} GiB RAM, {} ({}), {}, {} build",
            self.cpu,
            self.logical_cpus,
            self.memory_bytes as f64 / f64::from(1u32 << 30),
            self.os,
            self.arch,
            self.rustc,
            self.profile
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_known_dates() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_795_132_800), "2026-11-20T00:00:00Z");
        assert_eq!(rfc3339(1_795_132_800 + 3_661), "2026-11-20T01:01:01Z");
    }

    #[test]
    fn detect_fills_every_field() {
        let m = Machine::detect();
        assert!(m.logical_cpus > 0);
        assert!(m.rustc.starts_with("rustc "));
        assert!(!m.measured_at.is_empty());
    }
}
