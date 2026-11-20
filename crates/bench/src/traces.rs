//! Real traffic: public HTTP request logs, fetched when the benchmark runs.
//!
//! Nothing is written to disk. Each file is downloaded into memory, checked against a
//! pinned SHA-256, decompressed and parsed as a stream, and dropped when the run ends.
//! Client hosts are turned into anonymous numbers while parsing and the mapping is
//! discarded straight away: results contain aggregate numbers only, as the archive asks
//! ("please do not attempt any analysis beyond general traffic patterns").

use anyhow::{Context, bail};
use flate2::read::GzDecoder;
use ratelab::{Cost, Nanos};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sim::Arrival;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};

/// One downloadable trace file.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TraceFile {
    /// Where it is downloaded from.
    pub url: &'static str,
    /// Expected SHA-256 of the compressed file, hex.
    pub sha256: &'static str,
}

/// A public trace made of one or more files.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Dataset {
    /// Short name.
    pub name: &'static str,
    /// What it is.
    pub description: &'static str,
    /// Where it is described, including its terms.
    pub homepage: &'static str,
    /// The files, in time order.
    pub files: &'static [TraceFile],
}

/// Two months of every HTTP request to the NASA Kennedy Space Center web server
/// (July and August 1995), from the Internet Traffic Archive. "The traces may be
/// freely redistributed."
pub const NASA_KSC_1995: Dataset = Dataset {
    name: "NASA-HTTP (KSC, Jul–Aug 1995)",
    description: "every HTTP request to the NASA Kennedy Space Center web server, July and August 1995",
    homepage: "https://ita.ee.lbl.gov/html/contrib/NASA-HTTP.html",
    files: &[
        TraceFile {
            url: "https://ita.ee.lbl.gov/traces/NASA_access_log_Jul95.gz",
            sha256: "199109ed0f273e095da6ccd5fc9dc4cd8bb58daa06d62135e62090fea9d27488",
        },
        TraceFile {
            url: "https://ita.ee.lbl.gov/traces/NASA_access_log_Aug95.gz",
            sha256: "14995aed0ba4558ab832613ebea9a3ef2d87cb4297fc67f5694e0032bbb6b788",
        },
    ],
};

/// Downloads `file` into memory with `curl` (already on macOS, Linux and CI runners, so
/// no TLS library is added to the build) and checks its checksum. Nothing touches disk.
pub fn fetch(file: &TraceFile) -> anyhow::Result<Vec<u8>> {
    let out = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "600",
        ])
        .arg(file.url)
        .output()
        .context("run curl (is it installed?)")?;
    if !out.status.success() {
        bail!(
            "download {} failed: {}",
            file.url,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let digest: String = Sha256::digest(&out.stdout)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != file.sha256 {
        bail!(
            "{} has SHA-256 {digest}, expected {}; the file changed upstream",
            file.url,
            file.sha256
        );
    }
    Ok(out.stdout)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix seconds for a Common Log Format time: `01/Aug/1995:00:00:01 -0400`.
pub fn clf_seconds(s: &str) -> Option<i64> {
    let (date, zone) = s.split_once(' ')?;
    let mut parts = date.split([':', '/']);
    let day: i64 = parts.next()?.parse().ok()?;
    let name = parts.next()?;
    let month = MONTHS.iter().position(|m| *m == name)? as i64 + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let h: i64 = parts.next()?.parse().ok()?;
    let mi: i64 = parts.next()?.parse().ok()?;
    let sec: i64 = parts.next()?.parse().ok()?;
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let zh: i64 = zone.get(1..3)?.parse().ok()?;
    let zm: i64 = zone.get(3..5)?.parse().ok()?;
    let local = days_from_civil(year, month, day) * 86_400 + h * 3_600 + mi * 60 + sec;
    Some(local - sign * (zh * 3_600 + zm * 60))
}

/// Requests from a trace: time in Unix seconds and an anonymous client number.
#[derive(Debug, Default)]
pub struct Requests {
    /// (unix seconds, client) per request, in file order.
    pub rows: Vec<(i64, u32)>,
    /// Distinct clients.
    pub clients: u32,
    /// Lines that could not be read.
    pub skipped: u64,
}

/// Parses gzip-compressed Common Log Format lines. Hosts become numbers; the mapping
/// lives only inside this call.
pub fn parse_clf_gz(files: &[Vec<u8>]) -> anyhow::Result<Requests> {
    let mut ids: HashMap<String, u32> = HashMap::new();
    let mut out = Requests::default();
    for gz in files {
        // Latin-1 bytes appear in some request lines: read bytes, not UTF-8 strings.
        let mut reader = BufReader::new(GzDecoder::new(gz.as_slice()));
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&line);
            let parsed = text.split_once(' ').and_then(|(host, rest)| {
                let open = rest.find('[')?;
                let close = rest[open..].find(']')? + open;
                Some((host, clf_seconds(&rest[open + 1..close])?))
            });
            match parsed {
                Some((host, t)) => {
                    let next = ids.len() as u32;
                    let id = *ids.entry(host.to_owned()).or_insert(next);
                    out.rows.push((t, id));
                }
                None => out.skipped += 1,
            }
        }
    }
    out.clients = ids.len() as u32;
    Ok(out)
}

/// Sorted arrivals from time zero, one unit each. Requests in the same second keep
/// their file order (the logs have one-second resolution).
pub fn arrivals(requests: &Requests) -> Vec<Arrival> {
    let start = requests.rows.iter().map(|r| r.0).min().unwrap_or(0);
    let mut a: Vec<Arrival> = requests
        .rows
        .iter()
        .map(|&(t, key)| Arrival {
            at: Nanos(((t - start).max(0) as u64) * 1_000_000_000),
            key,
            cost: Cost::ONE,
        })
        .collect();
    a.sort_by_key(|x| x.at);
    a
}

/// Facts about the trace that was replayed (never per-client data).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceInfo {
    /// Dataset name.
    pub name: String,
    /// Where it is described.
    pub homepage: String,
    /// Files and their checksums.
    pub files: Vec<(String, String)>,
    /// Requests replayed.
    pub requests: u64,
    /// Distinct clients.
    pub clients: u32,
    /// Lines that could not be read.
    pub skipped: u64,
    /// From first to last request, seconds.
    pub span_secs: u64,
    /// Time resolution of the log, seconds.
    pub resolution_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    #[test]
    fn clf_time_with_zone() {
        // 01/Aug/1995:00:00:01 -0400 is 04:00:01 UTC.
        assert_eq!(clf_seconds("01/Aug/1995:00:00:01 -0400"), Some(807_249_601));
        assert_eq!(clf_seconds("31/Dec/1999:23:59:59 +0000"), Some(946_684_799));
        assert_eq!(clf_seconds("nonsense"), None);
    }

    #[test]
    fn parses_lines_and_anonymises_hosts() {
        let log = b"a.example - - [01/Aug/1995:00:00:01 -0400] \"GET / HTTP/1.0\" 200 1\n\
b.example - - [01/Aug/1995:00:00:03 -0400] \"GET /x HTTP/1.0\" 200 1\n\
garbage line\n\
a.example - - [01/Aug/1995:00:00:02 -0400] \"GET /y\xe9 HTTP/1.0\" 200 1\n";
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(log).unwrap();
        let gz = enc.finish().unwrap();
        let r = parse_clf_gz(&[gz]).unwrap();
        assert_eq!(r.clients, 2);
        assert_eq!(r.skipped, 1);
        assert_eq!(r.rows.iter().map(|x| x.1).collect::<Vec<_>>(), [0, 1, 0]);
        let a = arrivals(&r);
        assert_eq!(
            a.iter().map(|x| x.at.0 / 1_000_000_000).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }
}
