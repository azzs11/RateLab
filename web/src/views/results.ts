// Results: the benchmark's JSON files as tables and bars, with the machine and commit
// each was measured on. Nothing here is measured in the browser.

import { loadResults, machineLine, type Machine, type StrategyResults, type Summary } from "../lib/data";
import { h } from "../lib/dom";
import { fmtInt, fmtNum, fmtPct, fmtRate, pretty } from "../lib/format";

const REPO = "https://github.com/azzs11/distributed-rate-limiter";

interface Local {
  machine: Machine;
  size: { scenario_secs: number; seeds: number; runs: number; threads: number[]; keys: number[] };
  timer: { resolution_ns: number; overhead_ns: number };
  accuracy: { algorithm: string; scenario: string; over_pct: Summary; worst_window_ratio: Summary }[];
  latency: { algorithm: string; keys: number; batch: number; p50_ns: Summary; p99_ns: Summary; p999_ns: Summary }[];
  throughput: { algorithm: string; threads: number; keys: number; decisions_per_sec: Summary }[];
  fairness: { algorithm: string; keying: string; jain: Summary; noisy_share_pct: Summary }[];
}

interface Memory {
  machine: Machine;
  rows: { algorithm: string; keys: number; limit: number; bytes_per_key_one_request: number; bytes_per_key_full_window: number }[];
}

interface Cluster {
  machine: Machine;
  host: string | null;
  nodes: number;
  limit: number;
  keys: number;
  load_factor: number;
  run_secs: number;
  seeds: number;
  scenarios: {
    scenario: string;
    node0_share_pct: number;
    over_vs_single_pct: Summary;
    under_vs_single_pct: Summary;
    redis_calls_per_1k: Summary;
    redis_bytes_per_decision: Summary;
    p50_ms: Summary;
    p99_ms: Summary;
  }[];
  throughput: { concurrency: number; requests_per_sec: Summary; p99_ms: Summary } | null;
  outage_secs: [number, number];
  resilience: { phase: string; route: string; failure_policy: string; allowed_pct: number; redis_pct: number; breaker_open_pct: number; p99_ms: number }[];
  recovery_ms: number | null;
}

/** A table cell with a proportional bar behind the number. */
function barCell(value: number, max: number, text: string, kind = ""): HTMLElement {
  const pct = max > 0 ? Math.min(100, (value / max) * 100) : 0;
  return h(
    "td",
    { class: `n bar-cell ${kind}` },
    h("span", { class: "bar", style: `width:${pct.toFixed(1)}%`, "aria-hidden": "true" }),
    h("span", { class: "bar-text" }, text),
  );
}

function table(caption: string, head: string[], rows: (HTMLElement | string)[][]): HTMLElement {
  return h(
    "div",
    { class: "table-wrap" },
    h(
      "table",
      {},
      h("caption", { class: "table-title" }, caption),
      h("thead", {}, h("tr", {}, ...head.map((x, i) => h("th", { scope: "col", class: i ? "n" : "" }, x)))),
      h(
        "tbody",
        {},
        ...rows.map((r) =>
          h("tr", {}, ...r.map((c, i) => (typeof c === "string" ? (i === 0 ? h("th", { scope: "row" }, c) : h("td", { class: "n" }, c)) : c))),
        ),
      ),
    ),
  );
}

function block(id: string, title: string, intro: string, machine: Machine | null, ...body: (Node | null)[]): HTMLElement {
  return h(
    "section",
    { class: "section", "aria-labelledby": id },
    h(
      "div",
      { class: "page stack" },
      h("h2", { class: "display results-title", id }, title),
      h("p", {}, intro),
      machine ? h("p", { class: "note mono" }, `Measured on ${machineLine(machine)}`) : null,
      ...body,
    ),
  );
}

const missing = (what: string) => h("p", { class: "note" }, `${what} has not been measured for this build.`);

export default async function results(root: HTMLElement) {
  const [local, memory, cluster, strategies] = await Promise.all([
    loadResults<Local>("local"),
    loadResults<Memory>("memory"),
    loadResults<Cluster>("cluster"),
    loadResults<StrategyResults>("strategies"),
  ]);

  root.append(
    h(
      "div",
      { class: "page view-head" },
      h("p", { class: "kicker" }, "Results"),
      h("h1", { class: "display" }, "Measured, ", h("span", { class: "hero-em", style: "display:inline" }, "not claimed")),
      h(
        "p",
        { class: "lede" },
        "Everything below comes from one command, just results, and is copied unchanged from the JSON files in results/. Each section names the machine and commit it was measured on.",
      ),
      h(
        "p",
        {},
        h("a", { href: `${REPO}/blob/main/results/RESULTS.md` }, "Full tables in RESULTS.md"),
        " · ",
        h("a", { href: `${REPO}/tree/main/results` }, "Raw JSON"),
      ),
    ),
  );

  // Accuracy: worst window per algorithm and scenario.
  if (local) {
    const algos = [...new Set(local.accuracy.map((r) => r.algorithm))];
    const scenarios = [...new Set(local.accuracy.map((r) => r.scenario))];
    const maxWorst = Math.max(...local.accuracy.map((r) => r.worst_window_ratio.mean));
    root.append(
      block(
        "acc",
        "How far over the limit",
        `The most units one key got in any one-second span, as a multiple of the limit (100 per second). The exact rule never exceeds 1. ${local.size.scenario_secs} s per scenario, ${local.size.seeds} seeds.`,
        local.machine,
        table(
          "Worst window ÷ limit, per algorithm and traffic pattern",
          ["Traffic", ...algos.map(pretty)],
          scenarios.map((sc) => [
            sc.replace(/_/g, " "),
            ...algos.map((a) => {
              const r = local.accuracy.find((x) => x.algorithm === a && x.scenario === sc)!;
              const v = r.worst_window_ratio.mean;
              return barCell(v, maxWorst, `${v.toFixed(2)}×`, v > 1.01 ? "warn" : "");
            }),
          ]),
        ),
      ),
    );

    const maxP99 = Math.max(...local.latency.map((r) => r.p99_ns.mean));
    root.append(
      block(
        "lat",
        "How long a decision takes",
        `One thread, ${local.latency[0]?.keys ?? 0} keys, in batches of ${local.latency[0]?.batch ?? 0} calls (the clock resolution on the machine was ${local.timer.resolution_ns} ns). Mean over ${local.size.runs} runs.`,
        null,
        table(
          "Nanoseconds per decision",
          ["Algorithm", "p50", "p99", "p99.9"],
          local.latency.map((r) => [
            pretty(r.algorithm),
            barCell(r.p50_ns.mean, maxP99, fmtNum(r.p50_ns.mean)),
            barCell(r.p99_ns.mean, maxP99, fmtNum(r.p99_ns.mean)),
            h("td", { class: "n" }, fmtNum(r.p999_ns.mean)),
          ]),
        ),
      ),
    );

    const tputMax = Math.max(...local.throughput.map((r) => r.decisions_per_sec.mean));
    root.append(
      block(
        "tput",
        "How many decisions per second",
        "One limiter shared by every thread; keys picked at random. With a single key every thread contends for the same entry.",
        null,
        ...local.size.keys.map((k) =>
          table(
            `${fmtInt(k)} key${k === 1 ? "" : "s"}`,
            ["Algorithm", ...local.size.threads.map((t) => `${t} thread${t === 1 ? "" : "s"}`)],
            algos.map((a) => [
              pretty(a),
              ...local.size.threads.map((t) => {
                const r = local.throughput.find((x) => x.algorithm === a && x.threads === t && x.keys === k);
                return r ? barCell(r.decisions_per_sec.mean, tputMax, fmtRate(r.decisions_per_sec.mean)) : h("td", { class: "n" }, "–");
              }),
            ]),
          ),
        ),
      ),
    );

    root.append(
      block(
        "fair",
        "Who gets the capacity",
        "Ten tenants want more than an equal share; one wants ten times more. Jain's index is 1 when every tenant gets the same.",
        null,
        table(
          "Fairness",
          ["Algorithm", "Keying", "Jain's index", "Noisy tenant's share"],
          local.fairness.map((r) => [pretty(r.algorithm), h("td", {}, r.keying.replace("_", " ")), h("td", { class: "n" }, r.jain.mean.toFixed(3)), h("td", { class: "n" }, fmtPct(r.noisy_share_pct.mean, 1))]),
        ),
      ),
    );
  } else {
    root.append(h("div", { class: "page" }, missing("The in-process suite")));
  }

  if (memory) {
    const max = Math.max(...memory.rows.map((r) => r.bytes_per_key_full_window));
    root.append(
      block(
        "mem",
        "Memory per key",
        `Heap bytes per key with ${fmtInt(memory.rows[0]?.keys ?? 0)} keys, counted by the allocator, after one request and after a full window of ${memory.rows[0]?.limit ?? 0}.`,
        memory.machine,
        table(
          "Bytes per key",
          ["Algorithm", "After 1 request", "Full window"],
          memory.rows.map((r) => [
            pretty(r.algorithm),
            barCell(r.bytes_per_key_one_request, max, fmtInt(r.bytes_per_key_one_request)),
            barCell(r.bytes_per_key_full_window, max, fmtInt(r.bytes_per_key_full_window)),
          ]),
        ),
      ),
    );
  }

  if (cluster) {
    root.append(
      block(
        "cluster",
        `${cluster.nodes} nodes on one Redis`,
        `The Axum server on ${cluster.nodes} Docker nodes sharing a Redis token bucket; ${cluster.keys} keys at ${cluster.load_factor}× the limit of ${cluster.limit} per second, ${cluster.run_secs} s, ${cluster.seeds} seeds. "vs one bucket" compares with a single perfectly coordinated bucket fed the same send times.${cluster.host ? ` Host: ${cluster.host}.` : ""}`,
        cluster.machine,
        table(
          "Accuracy, cost and latency",
          ["Scenario", "Over vs one bucket", "Under vs one bucket", "Redis calls / 1k", "Bytes / decision", "p50", "p99"],
          cluster.scenarios.map((s) => [
            `${s.scenario} (${s.node0_share_pct.toFixed(0)}% on node 1)`,
            fmtPct(s.over_vs_single_pct.mean),
            fmtPct(s.under_vs_single_pct.mean),
            fmtInt(s.redis_calls_per_1k.mean),
            fmtInt(s.redis_bytes_per_decision.mean),
            `${s.p50_ms.mean.toFixed(2)} ms`,
            `${s.p99_ms.mean.toFixed(2)} ms`,
          ]),
        ),
        cluster.throughput
          ? h(
              "p",
              {},
              `Closed loop with ${cluster.throughput.concurrency} requests in flight: ${fmtInt(cluster.throughput.requests_per_sec.mean)} requests per second, p99 ${cluster.throughput.p99_ms.mean.toFixed(2)} ms.`,
            )
          : null,
        table(
          `Redis outage from ${cluster.outage_secs[0]} s to ${cluster.outage_secs[1]} s`,
          ["Phase", "Route (policy)", "Allowed", "Decided by Redis", "Breaker open", "p99"],
          cluster.resilience.map((r) => [
            r.phase,
            h("td", {}, `${r.route} (fail ${r.failure_policy})`),
            fmtPct(r.allowed_pct, 1),
            fmtPct(r.redis_pct, 1),
            fmtPct(r.breaker_open_pct, 1),
            `${r.p99_ms.toFixed(1)} ms`,
          ]),
        ),
        cluster.recovery_ms !== null
          ? h("p", { class: "note" }, `Every node was answering from Redis again ${fmtInt(cluster.recovery_ms)} ms after Redis recovered.`)
          : h("p", { class: "note" }, "Not every node had recovered by the end of the run."),
      ),
    );
  }

  if (strategies) {
    root.append(
      block(
        "strat",
        "Coordination strategies under stress",
        `The simulator, ${strategies.size.seeds} seeds per row. Errors are % of what one perfectly coordinated limiter admits.`,
        strategies.machine,
        h("p", {}, "Every strategy on every traffic pattern: ", h("a", { href: "#/explorer" }, "the strategy explorer"), "."),
        table(
          "Clock skew of −50, 0 and +50 ms between 3 nodes",
          ["Strategy", "Error, no skew", "Error, skewed"],
          strategies.skew.map((r) => [pretty(r.strategy), fmtPct(r.error_no_skew_pct.mean), fmtPct(r.error_skewed_pct.mean)]),
        ),
        table(
          "Store outage for the middle fifth of the run",
          ["Strategy", "Policy", "Allowed during", "Over during", "Under during"],
          strategies.outage.map((r) => [
            pretty(r.strategy),
            h("td", {}, `fail ${r.policy}`),
            fmtPct(r.during_allowed_pct.mean, 1),
            fmtPct(r.during_over_pct.mean, 1),
            fmtPct(r.during_under_pct.mean, 1),
          ]),
        ),
        table(
          "All traffic moves from node 1 to node 2 halfway through",
          ["Strategy", "Under-admission, first second", "Rebalance time"],
          strategies.convergence.map((r) => [
            pretty(r.strategy),
            fmtPct(r.under_first_second_pct.mean, 1),
            r.rebalance_ms.n === 0 ? "never" : `${fmtInt(r.rebalance_ms.mean)} ms${r.never ? ` (${r.never} never)` : ""}`,
          ]),
        ),
      ),
    );
  }
  root.append(h("div", { class: "spacer" }));
  return () => {};
}
