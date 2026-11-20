//! Prose with measured numbers: the report, the article, the portfolio card and the
//! résumé bullets are written as Markdown templates with `{{name}}` placeholders, and
//! every number is filled in here from the result files.
//!
//! The rule the README follows (no number unless code produced it) holds for the
//! prose too: a template can only use names in [`facts`], and an unknown name is an
//! error, so a number can never be typed in by hand or go stale silently.

use crate::report::{Inputs, sim_gap};
use crate::strategies::dominated_by;
use anyhow::bail;
use std::collections::BTreeMap;

/// Named, formatted numbers for the templates.
pub type Facts = BTreeMap<String, String>;

fn f(v: f64, d: usize) -> String {
    format!("{v:.d$}")
}

/// Whole numbers with thousands separators: 3461612 → "3,461,612".
pub fn thousands(v: f64) -> String {
    let n = v.round() as i64;
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// Every fact the result files support. Missing files just mean fewer facts.
pub fn facts(inputs: &Inputs) -> Facts {
    let mut m = Facts::new();
    let mut put = |k: &str, v: String| {
        m.insert(k.to_owned(), v);
    };

    if let Some(l) = &inputs.local {
        put("machine", l.machine.one_line());
        put(
            "machine.cpu",
            format!("{} ({} cores)", l.machine.cpu, l.machine.logical_cpus),
        );
        put("date", l.machine.measured_at.chars().take(10).collect());
        let acc = |alg: &str, sc: &str| {
            l.accuracy
                .iter()
                .find(|r| r.algorithm == alg && r.scenario == sc)
        };
        if let Some(r) = acc("fixed_window", "boundary_attack") {
            put("attack.fixed.worst", f(r.worst_window_ratio.mean, 2));
        }
        if let Some(r) = acc("token_bucket", "steady_poisson") {
            put("steady.bucket.over_pct", f(r.over_pct.mean, 1));
            put("steady.bucket.worst", f(r.worst_window_ratio.mean, 2));
        }
        let p50: Vec<f64> = l.latency.iter().map(|r| r.p50_ns.mean).collect();
        if !p50.is_empty() {
            put(
                "latency.p50.min_ns",
                f(p50.iter().copied().fold(f64::INFINITY, f64::min), 0),
            );
            put(
                "latency.p50.max_ns",
                f(p50.iter().copied().fold(0.0, f64::max), 0),
            );
            put("latency.keys", thousands(l.latency[0].keys as f64));
        }
        let tput = |alg: &str, threads: usize| {
            l.throughput
                .iter()
                .find(|r| r.algorithm == alg && r.threads == threads && r.keys == 1_000)
        };
        if let Some(r) = tput("gcra", 1) {
            put("tput.gcra.1t", f(r.decisions_per_sec.mean / 1e6, 0));
        }
        let top = l
            .throughput
            .iter()
            .filter(|r| r.keys == 1_000)
            .max_by(|a, b| {
                a.decisions_per_sec
                    .mean
                    .total_cmp(&b.decisions_per_sec.mean)
            });
        if let Some(r) = top {
            put("tput.max", f(r.decisions_per_sec.mean / 1e6, 0));
            put("tput.max.threads", r.threads.to_string());
            put("tput.max.algorithm", r.algorithm.replace('_', " "));
        }
    }

    if let Some(mem) = &inputs.memory {
        let b = |a: &str| {
            mem.rows
                .iter()
                .find(|r| r.algorithm == a)
                .map(|r| r.bytes_per_key_full_window)
        };
        if let (Some(g), Some(s), Some(t)) = (b("gcra"), b("sliding_window"), b("token_bucket")) {
            put("mem.gcra", f(g, 0));
            put("mem.sliding", thousands(s));
            put("mem.token_bucket", f(t, 0));
            put("mem.sliding_over_gcra", f(s / g, 0));
        }
    }

    if let Some(st) = &inputs.strategies {
        put("strat.seeds", st.size.seeds.to_string());
        put("strat.secs", st.size.secs.to_string());
        put("strat.delay_us", st.network.one_way_us.to_string());
        put("strat.limit", st.limit.to_string());
        put("strat.keys", st.keys.to_string());
        for r in st.matrix.iter().filter(|r| r.nodes == 3) {
            let k = format!("s3.{}.{}", r.strategy, r.scenario);
            put(&format!("{k}.err"), f(r.error_pct.mean, 2));
            put(&format!("{k}.calls"), f(r.calls_per_1k.mean, 0));
            put(
                &format!("{k}.saved_pct"),
                f(100.0 - r.calls_per_1k.mean / 10.0, 0),
            );
            put(&format!("{k}.p99_us"), f(r.p99_us.mean, 0));
        }
        let beaten = dominated_by(&st.matrix, "adaptive_leasing");
        let configs = st
            .matrix
            .iter()
            .filter(|r| r.strategy == "adaptive_leasing")
            .count();
        put("adaptive.beaten", beaten.len().to_string());
        put("adaptive.configs", configs.to_string());
        put(
            "adaptive.beaten_multinode",
            beaten.iter().filter(|d| d.nodes > 1).count().to_string(),
        );
        let multi: Vec<_> = st
            .matrix
            .iter()
            .filter(|r| r.strategy == "adaptive_leasing" && r.nodes > 1)
            .collect();
        let fixed = |r: &crate::strategies::StrategyRow| {
            st.matrix.iter().find(|x| {
                x.strategy == "fixed_leasing" && x.scenario == r.scenario && x.nodes == r.nodes
            })
        };
        put("adaptive.multinode", multi.len().to_string());
        put(
            "adaptive.fewer_calls_than_fixed",
            multi
                .iter()
                .filter(|r| fixed(r).is_some_and(|x| r.calls_per_1k.mean < x.calls_per_1k.mean))
                .count()
                .to_string(),
        );
        put(
            "adaptive.less_error_than_fixed",
            multi
                .iter()
                .filter(|r| fixed(r).is_some_and(|x| r.error_pct.mean < x.error_pct.mean))
                .count()
                .to_string(),
        );
        let skew = crate::strategies::SKEW_MS
            .iter()
            .map(|x| x.unsigned_abs())
            .max()
            .unwrap_or(0);
        put("skew.ms", skew.to_string());
        if let Some(r) = st
            .skew
            .iter()
            .find(|r| r.strategy == "central_exact_node_clock")
        {
            put("skew.node_clock.err", f(r.error_skewed_pct.mean, 0));
        }
        if let Some(r) = st.skew.iter().find(|r| r.strategy == "adaptive_leasing") {
            put("skew.adaptive.err", f(r.error_skewed_pct.mean, 2));
        }
        if let Some(r) = st
            .outage
            .iter()
            .find(|r| r.strategy == "local_split" && r.policy == "open")
        {
            put("outage.local_split.over", f(r.during_over_pct.mean, 1));
        }
        for s in ["adaptive_leasing", "fixed_leasing"] {
            if let Some(r) = st.convergence.iter().find(|r| r.strategy == s) {
                put(&format!("conv.{s}.ms"), f(r.rebalance_ms.mean, 0));
            }
        }
    }

    if let Some(r) = &inputs.real_strategies {
        let (err, calls) = sim_gap(r);
        put("realcheck.err_gap_pp", f(err, 2));
        put("realcheck.calls_gap_pct", f(calls, 1));
        put("realcheck.nodes", r.nodes.to_string());
        put("realcheck.seeds", r.seeds.to_string());
    }

    if let Some(c) = &inputs.cluster {
        put("cluster.nodes", c.nodes.to_string());
        if let Some(u) = c.scenarios.iter().find(|r| r.scenario == "uniform") {
            put("cluster.over_pct", f(u.over_vs_single_pct.mean, 2));
            put("cluster.p99_ms", f(u.p99_ms.mean, 2));
        }
        if let Some(t) = &c.throughput {
            put("cluster.rps", thousands(t.requests_per_sec.mean));
        }
    }

    if let Some(t) = &inputs.traces {
        put("trace.requests", thousands(t.trace.requests as f64));
        put("trace.clients", thousands(f64::from(t.trace.clients)));
        put("trace.days", (t.trace.span_secs / 86_400).to_string());
        put("trace.limit", t.limit.to_string());
        put("trace.window_s", t.window_secs.to_string());
        put("trace.target_pct", f(t.target_blocked_pct, 0));
        for r in &t.strategies {
            let k = format!("trace.{}.{}", r.strategy, r.nodes);
            put(&format!("{k}.calls"), f(r.calls_per_1k, 0));
            put(&format!("{k}.err"), f(r.error_pct, 2));
        }
        if let Some(r) = t.algorithms.iter().find(|r| r.algorithm == "fixed_window") {
            put("trace.fixed.worst", f(r.worst_window_ratio, 2));
        }
    }

    if let Some(o) = &inputs.overload {
        put("ov.workers", o.workers.to_string());
        put("ov.service_ms", o.service_ms.to_string());
        put("ov.deadline_ms", o.deadline_ms.to_string());
        put(
            "ov.capacity",
            (o.workers as u64 * 1_000 / o.service_ms.max(1)).to_string(),
        );
        put("ov.run_s", o.size.run_secs.to_string());
        let mut gap: f64 = 0.0;
        for sc in &o.scenarios {
            let short = if sc.name == "ramp" { "ramp" } else { "cap" };
            if sc.name == "ramp"
                && let Some(last) = sc.load.last()
            {
                let cap = o.workers as f64 * 1_000.0 / o.service_ms.max(1) as f64;
                put("ov.ramp.peak_x", f(last.1 as f64 / cap, 0));
            }
            for r in &sc.rows {
                let a = r.real.as_ref().unwrap_or(&r.sim);
                let k = format!("ov.{short}.{}", r.limiter);
                put(&format!("{k}.goodput"), f(a.goodput_pct.mean, 0));
                put(&format!("{k}.p99_ms"), f(a.p99_ms.mean, 0));
                put(&format!("{k}.timeout_pct"), f(a.timeout_pct.mean, 0));
                if let Some(real) = &r.real {
                    gap = gap.max((real.goodput_pct.mean - r.sim.goodput_pct.mean).abs());
                }
            }
        }
        put("ov.gap_pp", f(gap, 1));
    }

    if let Some(l) = &inputs.llm {
        put("llm.tenants", l.tenants.to_string());
        put("llm.citation", l.citation.clone());
        put("llm.target_pct", f(l.target_blocked_pct, 0));
        if let Some(a) = l.scenarios.iter().find(|s| s.name == "azure_2023") {
            put("llm.requests", thousands(a.requests as f64));
            put("llm.minutes", f(a.span_secs / 60.0, 0));
            put("llm.oracle_blocked", f(a.oracle_blocked_pct.mean, 1));
            for r in &a.rows {
                let k = format!("llm.{}", r.accounting);
                put(&format!("{k}.blocked"), f(r.blocked_pct.mean, 1));
                put(&format!("{k}.wrong"), f(r.wrongly_blocked_pct.mean, 1));
                put(&format!("{k}.overspend"), f(r.overspend_pct.mean, 2));
                put(&format!("{k}.debts"), f(r.debts.mean, 0));
                put(&format!("{k}.repaid_ms"), f(r.recovery_mean_ms.mean, 0));
            }
        }
        if let Some(r) = &l.real {
            put("llm.real.minutes", r.trace_minutes.to_string());
            let gap = r
                .rows
                .iter()
                .map(|x| (x.blocked_pct.0 - x.blocked_pct.1).abs())
                .fold(0.0, f64::max);
            put("llm.real.blocked_gap_pp", f(gap, 1));
        }
    }
    m
}

/// Fills `{{name}}` placeholders. Unknown names are an error that lists them.
///
/// ```
/// use bench::story::{render, Facts};
/// let mut f = Facts::new();
/// f.insert("x".into(), "42".into());
/// assert_eq!(render("x is {{x}}.", &f).unwrap(), "x is 42.");
/// assert!(render("{{y}}", &f).is_err());
/// ```
pub fn render(template: &str, facts: &Facts) -> anyhow::Result<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    let mut unknown = Vec::new();
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            bail!("unclosed {{{{ in template");
        };
        let name = after[..end].trim();
        match facts.get(name) {
            Some(v) => out.push_str(v),
            None => unknown.push(name.to_owned()),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    if !unknown.is_empty() {
        bail!(
            "unknown facts in template: {} (list them with `bench facts`)",
            unknown.join(", ")
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(3_461_612.0), "3,461,612");
        assert_eq!(thousands(999.0), "999");
        assert_eq!(thousands(-1_000.0), "-1,000");
    }

    #[test]
    fn unknown_names_are_listed() {
        let e = render("{{a}} {{b}}", &Facts::new())
            .unwrap_err()
            .to_string();
        assert!(e.contains("a, b"), "{e}");
        assert!(render("{{oops", &Facts::new()).is_err());
    }

    #[test]
    fn no_inputs_no_facts() {
        assert!(facts(&Inputs::default()).is_empty());
    }
}
