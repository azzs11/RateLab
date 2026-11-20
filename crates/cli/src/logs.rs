//! Reading access logs: nginx/Apache combined (or common) format, CSV with a header, or
//! JSON Lines. Each line becomes an arrival: a time and a caller. Callers are numbered in
//! order of first appearance; the names are kept only so the CLI can print the busiest.

use anyhow::{Context, bail};
use ratelab::{Cost, Nanos};
use sim::Arrival;
use std::collections::HashMap;

/// Log formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// Detect from the first line.
    Auto,
    /// nginx / Apache combined or common log format: the client is the first field.
    Clf,
    /// Comma-separated with a header row.
    Csv,
    /// One JSON object per line.
    Jsonl,
}

/// Column names tried for the time, in order.
const TIME_NAMES: [&str; 6] = ["time", "timestamp", "ts", "date", "datetime", "@timestamp"];
/// Column names tried for the caller, in order.
const KEY_NAMES: [&str; 10] = [
    "key",
    "api_key",
    "user",
    "user_id",
    "client",
    "client_ip",
    "ip",
    "tenant",
    "host",
    "remote_addr",
];

/// A parsed log.
#[derive(Debug, Default)]
pub struct Log {
    /// Requests, sorted by time, from zero.
    pub arrivals: Vec<Arrival>,
    /// Caller names by key number.
    pub names: Vec<String>,
    /// Lines that could not be read.
    pub skipped: usize,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn zone_offset_ns(z: &str) -> Option<i64> {
    let z = z.trim();
    if z.is_empty() || z == "Z" || z == "z" {
        return Some(0);
    }
    let sign = match z.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = z[1..].chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 4 {
        return None;
    }
    let h: i64 = digits[..2].parse().ok()?;
    let m: i64 = digits[2..].parse().ok()?;
    Some(sign * (h * 3_600 + m * 60) * 1_000_000_000)
}

fn fraction_ns(f: &str) -> Option<i64> {
    if f.is_empty() {
        return Some(0);
    }
    if !f.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let digits: String = f.chars().take(9).collect();
    format!("{digits:0<9}").parse().ok()
}

/// Unix nanoseconds for a time written as Unix seconds or milliseconds (by size),
/// RFC 3339 / ISO 8601 (`2024-05-01T12:00:00.5Z`, a space instead of `T` too), or the
/// Common Log Format (`01/Aug/1995:00:00:01 -0400`).
///
/// ```
/// use ratelab_cli::logs::parse_time;
/// assert_eq!(parse_time("1700000000"), Some(1_700_000_000_000_000_000));
/// assert_eq!(parse_time("1700000000500"), Some(1_700_000_000_500_000_000));
/// assert_eq!(parse_time("2023-11-14T22:13:20Z"), Some(1_700_000_000_000_000_000));
/// assert_eq!(parse_time("14/Nov/2023:23:13:20 +0100"), Some(1_700_000_000_000_000_000));
/// assert_eq!(parse_time("yesterday"), None);
/// ```
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s
        .trim()
        .trim_matches('"')
        .trim_matches('[')
        .trim_matches(']');
    if s.is_empty() {
        return None;
    }
    // Whole numbers exactly; fractions through floating point.
    if let Ok(n) = s.parse::<i64>() {
        if n < 0 {
            return None;
        }
        return Some(if n >= 100_000_000_000 {
            n.saturating_mul(1_000_000)
        } else {
            n.saturating_mul(1_000_000_000)
        });
    }
    if let Ok(x) = s.parse::<f64>() {
        if !x.is_finite() || x < 0.0 {
            return None;
        }
        // Milliseconds once the number is too large to be seconds (after the year 5138).
        let ns = if x >= 1e11 { x * 1e6 } else { x * 1e9 };
        return Some(ns.round() as i64);
    }
    // Common Log Format: 01/Aug/1995:00:00:01 -0400
    if let Some((date, rest)) = s.split_once(':')
        && date.contains('/')
    {
        let mut d = date.split('/');
        let day: i64 = d.next()?.parse().ok()?;
        let name = d.next()?;
        let month = MONTHS.iter().position(|m| *m == name)? as i64 + 1;
        let year: i64 = d.next()?.parse().ok()?;
        let (clock, zone) = rest.split_once(' ').unwrap_or((rest, ""));
        let mut c = clock.split(':');
        let (h, mi, se): (i64, i64, i64) = (
            c.next()?.parse().ok()?,
            c.next()?.parse().ok()?,
            c.next()?.parse().ok()?,
        );
        let local =
            (days_from_civil(year, month, day) * 86_400 + h * 3_600 + mi * 60 + se) * 1_000_000_000;
        return Some(local - zone_offset_ns(zone)?);
    }
    // ISO 8601 / RFC 3339
    let (date, rest) = s.split_once(['T', ' '])?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let zone_at = rest
        .find(['Z', 'z', '+'])
        .or_else(|| rest.rfind('-').filter(|&i| i > 0));
    let (clock, zone) = match zone_at {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let (hms, frac) = clock.split_once('.').unwrap_or((clock, ""));
    let mut c = hms.split(':');
    let (h, mi): (i64, i64) = (c.next()?.parse().ok()?, c.next()?.parse().ok()?);
    let se: i64 = c.next().map_or(Some(0), |x| x.parse().ok())?;
    let local = (days_from_civil(y, m, day) * 86_400 + h * 3_600 + mi * 60 + se) * 1_000_000_000
        + fraction_ns(frac)?;
    Some(local - zone_offset_ns(zone)?)
}

/// Splits one CSV line, honouring double quotes.
fn csv_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut field)),
            _ => field.push(c),
        }
    }
    out.push(field);
    out
}

fn find_column(header: &[String], wanted: Option<&str>, names: &[&str]) -> Option<usize> {
    let lower: Vec<String> = header
        .iter()
        .map(|h| h.trim().to_ascii_lowercase())
        .collect();
    match wanted {
        Some(w) => lower.iter().position(|h| *h == w.to_ascii_lowercase()),
        None => names.iter().find_map(|n| lower.iter().position(|h| h == n)),
    }
}

fn detect(first: &str) -> Format {
    let t = first.trim_start();
    if t.starts_with('{') {
        Format::Jsonl
    } else if t.contains(" [") && t.contains("] \"") {
        Format::Clf
    } else {
        Format::Csv
    }
}

/// Parses `text`. `time_column` and `key_column` pick CSV columns or JSON fields by
/// name; otherwise common names are tried.
pub fn parse(
    text: &str,
    format: Format,
    time_column: Option<&str>,
    key_column: Option<&str>,
) -> anyhow::Result<Log> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty()).peekable();
    let Some(first) = lines.peek().copied() else {
        bail!("the log is empty");
    };
    let format = if format == Format::Auto {
        detect(first)
    } else {
        format
    };
    let mut ids: HashMap<String, u32> = HashMap::new();
    let mut names = Vec::new();
    let mut rows: Vec<(i64, u32)> = Vec::new();
    let mut skipped = 0;
    let mut add = |t: Option<i64>, key: Option<String>| match (t, key) {
        (Some(t), Some(k)) => {
            let next = ids.len() as u32;
            let id = *ids.entry(k.clone()).or_insert_with(|| {
                names.push(k);
                next
            });
            rows.push((t, id));
        }
        _ => skipped += 1,
    };
    match format {
        Format::Clf => {
            for line in lines {
                let key = line.split_whitespace().next().map(str::to_owned);
                let time = line
                    .find('[')
                    .and_then(|a| line[a..].find(']').map(|b| &line[a + 1..a + b]))
                    .and_then(parse_time);
                add(time, key);
            }
        }
        Format::Csv => {
            let header = csv_fields(lines.next().unwrap_or_default());
            let tc = find_column(&header, time_column, &TIME_NAMES)
                .with_context(|| format!("no time column in {header:?} (use --time-column)"))?;
            let kc = find_column(&header, key_column, &KEY_NAMES)
                .with_context(|| format!("no caller column in {header:?} (use --key-column)"))?;
            for line in lines {
                let f = csv_fields(line);
                add(
                    f.get(tc).and_then(|t| parse_time(t)),
                    f.get(kc).map(|k| k.trim().to_owned()),
                );
            }
        }
        Format::Jsonl => {
            for line in lines {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    add(None, None);
                    continue;
                };
                let pick = |wanted: Option<&str>, names: &[&str]| {
                    let obj = v.as_object()?;
                    let key = match wanted {
                        Some(w) => obj.keys().find(|k| k.eq_ignore_ascii_case(w))?,
                        None => names
                            .iter()
                            .find_map(|n| obj.keys().find(|k| k.eq_ignore_ascii_case(n)))?,
                    };
                    Some(match &obj[key] {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                };
                let time = pick(time_column, &TIME_NAMES).and_then(|t| parse_time(&t));
                add(time, pick(key_column, &KEY_NAMES));
            }
        }
        Format::Auto => unreachable!("resolved above"),
    }
    let start = rows.iter().map(|r| r.0).min().unwrap_or(0);
    let mut arrivals: Vec<Arrival> = rows
        .iter()
        .map(|&(t, key)| Arrival {
            at: Nanos(u64::try_from(t - start).unwrap_or(0)),
            key,
            cost: Cost::ONE,
        })
        .collect();
    arrivals.sort_by_key(|a| a.at);
    if arrivals.is_empty() {
        bail!("no line could be read ({skipped} skipped)");
    }
    Ok(Log {
        arrivals,
        names,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_combined_log_format() {
        let log = r#"10.0.0.1 - - [01/Aug/1995:00:00:01 -0400] "GET / HTTP/1.0" 200 7
10.0.0.2 - - [01/Aug/1995:00:00:03 -0400] "GET /a HTTP/1.0" 200 7
garbage
10.0.0.1 - - [01/Aug/1995:00:00:02 -0400] "GET /b HTTP/1.0" 200 7
"#;
        let l = parse(log, Format::Auto, None, None).unwrap();
        assert_eq!(l.arrivals.len(), 3);
        assert_eq!(l.skipped, 1);
        assert_eq!(l.names, ["10.0.0.1", "10.0.0.2"]);
        assert_eq!(
            l.arrivals.iter().map(|a| a.key).collect::<Vec<_>>(),
            [0, 0, 1]
        );
        assert_eq!(l.arrivals[2].at, Nanos(2_000_000_000));
    }

    #[test]
    fn reads_csv_with_named_columns_and_quotes() {
        let log = "timestamp,user,path\n2024-05-01T12:00:00Z,alice,\"/a,b\"\n2024-05-01T12:00:00.250Z,bob,/c\n";
        let l = parse(log, Format::Auto, None, None).unwrap();
        assert_eq!(l.arrivals.len(), 2);
        assert_eq!(l.arrivals[1].at, Nanos(250_000_000));
        let custom = "when,who\n1700000000,x\n1700000001,y\n";
        let l = parse(custom, Format::Csv, Some("when"), Some("who")).unwrap();
        assert_eq!(l.names, ["x", "y"]);
        assert!(
            parse(custom, Format::Csv, None, None).is_err(),
            "no known column names"
        );
    }

    #[test]
    fn reads_json_lines() {
        let log = "{\"ts\": 1700000000.5, \"api_key\": \"k1\"}\n{\"ts\": 1700000001, \"api_key\": 7}\nnot json\n";
        let l = parse(log, Format::Auto, None, None).unwrap();
        assert_eq!(l.arrivals.len(), 2);
        assert_eq!(l.names, ["k1", "7"]);
        assert_eq!(l.skipped, 1);
    }

    #[test]
    fn times_with_offsets_and_fractions() {
        assert_eq!(
            parse_time("2023-11-14 22:13:20.5+00:00"),
            Some(1_700_000_000_500_000_000)
        );
        assert_eq!(
            parse_time("2023-11-14T23:13:20+01:00"),
            Some(1_700_000_000_000_000_000)
        );
        assert_eq!(
            parse_time("2023-11-14T21:13:20-01:00"),
            Some(1_700_000_000_000_000_000)
        );
        assert_eq!(parse_time("-5"), None);
    }

    #[test]
    fn an_empty_log_is_an_error() {
        assert!(parse("", Format::Auto, None, None).is_err());
        assert!(parse("a - - [bad] x\n", Format::Clf, None, None).is_err());
    }
}
