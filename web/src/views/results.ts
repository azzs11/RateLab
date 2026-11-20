// Results: the benchmark's JSON files as tables and bars, with the machine and commit
// each was measured on. Nothing here is measured in the browser.

import { loadResults, machineLine, type Machine, type StrategyResults, type Summary } from "../lib/data";
import { afterPaint, h } from "../lib/dom";
import { masthead } from "../components/masthead";
import { fmtInt, fmtNum, fmtPct, fmtRate, pretty } from "../lib/format";
import { goodputChart, type GoodputLine } from "../components/goodput";

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
      { class: "page" },
      h(
        "div",
        { class: "prose" },
        h("h2", { id }, title),
        intro ? h("p", {}, intro) : null,
        machine ? h("aside", { class: "side" }, `Measured on ${machineLine(machine)}`) : null,
        h("div", { class: "wide stack" }, ...body),
      ),
    ),
  );
}

const missing = (what: string) => h("p", { class: "note" }, `${what} has not been measured for this build.`);

interface Traces {
  machine: Machine;
  trace: { name: string; homepage: string; requests: number; clients: number; span_secs: number };
  window_secs: number;
  limit: number;
  target_blocked_pct: number;
  algorithms: { algorithm: string; blocked_pct: number; over_pct: number; worst_window_ratio: number }[];
  strategies: { strategy: string; nodes: number; error_pct: number; calls_per_1k: number }[];
}

interface RealStrategies {
  machine: Machine;
  nodes: number;
  seeds: number;
  rows: { strategy: string; scenario: string; real_error_pct: Summary; sim_error_pct: Summary; real_calls_per_1k: Summary; sim_calls_per_1k: Summary }[];
}

interface OverloadAgg {
  goodput_pct: Summary;
  p99_ms: Summary;
  shed_pct: Summary;
  timeout_pct: Summary;
}

interface Overload {
  machine: Machine;
  size: { run_secs: number; sim_seeds: number; real_seeds: number };
  workers: number;
  service_ms: number;
  deadline_ms: number;
  scenarios: {
    name: string;
    description: string;
    rows: { limiter: string; sim: OverloadAgg; real: OverloadAgg | null }[];
    series: (GoodputLine & { source: string })[];
  }[];
}

function overloadBlock(o: Overload): HTMLElement {
  const capacity = Math.round((o.workers * 1000) / o.service_ms);
  const body: HTMLElement[] = [];
  for (const sc of o.scenarios) {
    const source = sc.series.some((l) => l.source === "real") ? "real" : "simulated";
    const lines = sc.series.filter((l) => l.source === source);
    const title = `${pretty(sc.name)}: successful answers per second (${source}, seed 1)`;
    const best = Math.max(...sc.rows.map((r) => (r.real ?? r.sim).goodput_pct.mean));
    body.push(
      h(
        "figure",
        { class: "stack" },
        h("h3", {}, pretty(sc.name)),
        h("p", {}, `${sc.description.charAt(0).toUpperCase()}${sc.description.slice(1)}.`),
        goodputChart(lines, title),
        h("figcaption", { class: "note" }, title),
      ),
      table(
        `${pretty(sc.name)}: goodput is % of what could have succeeded`,
        ["Limit", "Goodput real", "Goodput sim", "p99 real", "p99 sim", "Shed", "Timed out"],
        sc.rows.map((r) => {
          const a = r.real ?? r.sim;
          return [
            pretty(r.limiter),
            barCell(a.goodput_pct.mean, best, r.real ? fmtPct(r.real.goodput_pct.mean, 1) : "–"),
            fmtPct(r.sim.goodput_pct.mean, 1),
            r.real ? `${fmtInt(r.real.p99_ms.mean)} ms` : "–",
            `${fmtInt(r.sim.p99_ms.mean)} ms`,
            fmtPct(a.shed_pct.mean, 1),
            fmtPct(a.timeout_pct.mean, 1),
          ];
        }),
      ),
    );
  }
  return block(
    "overload",
    "Overload, with and without a concurrency limit",
    `A backend with ${o.workers} workers and a ${o.service_ms} ms mean service time (${fmtInt(capacity)} requests/s) behind a server that answers 504 after ${o.deadline_ms} ms, and never cancels late work. Every limit gets the same requests, ${o.size.sim_seeds} seed${o.size.sim_seeds === 1 ? "" : "s"} in the simulator and ${o.size.real_seeds} on the server's real concurrency layer, ${o.size.run_secs} s each. The static limit is the healthy worker count.`,
    o.machine,
    ...body,
  );
}

export default async function results(root: HTMLElement) {
  root.append(
    masthead(
      "Appendix",
      "Results",
      h(
        "span",
        {},
        "Everything below comes from one command, ",
        h("code", {}, "just results"),
        ", read unchanged from the JSON files in results/. Each section names the machine and commit it was measured on. ",
        h("a", { href: `${REPO}/blob/main/results/RESULTS.md` }, "Full tables"),
        " · ",
        h("a", { href: `${REPO}/tree/main/results` }, "raw JSON"),
        ".",
      ),
    ),
  );
  await afterPaint();
  const [local, memory, cluster, strategies, traces, real, overload] = await Promise.all([
    loadResults<Local>("local"),
    loadResults<Memory>("memory"),
    loadResults<Cluster>("cluster"),
    loadResults<StrategyResults>("strategies"),
    loadResults<Traces>("traces"),
    loadResults<RealStrategies>("real_strategies"),
    loadResults<Overload>("overload"),
  ]);

  
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
  if (traces) {
    root.append(
      block(
        "traces",
        "Real traffic",
        `${fmtInt(traces.trace.requests)} requests from ${fmtInt(traces.trace.clients)} clients over ${Math.round(traces.trace.span_secs / 86400)} days: ${traces.trace.name}. Downloaded into memory when the benchmark runs and discarded after; nothing is stored. The limit, ${traces.limit} per ${traces.window_secs} s per client, is the smallest at which the exact rule blocks at most ${traces.target_blocked_pct}% of requests.`,
        traces.machine,
        h("p", {}, h("a", { href: traces.trace.homepage }, "About the dataset and its terms")),
        table(
          "Every algorithm on the real log",
          ["Algorithm", "Blocked", "Over vs exact", "Worst window ÷ limit"],
          traces.algorithms.map((r) => [pretty(r.algorithm), fmtPct(r.blocked_pct), fmtPct(r.over_pct, 3), `${r.worst_window_ratio.toFixed(2)}×`]),
        ),
        table(
          "Every strategy on the real log (simulated cluster)",
          ["Strategy", "Nodes", "Admission error", "Store calls / 1k"],
          traces.strategies.map((r) => [pretty(r.strategy), String(r.nodes), fmtPct(r.error_pct), fmtInt(r.calls_per_1k)]),
        ),
      ),
    );
  }

  if (real) {
    root.append(
      block(
        "real",
        "The simulator, checked",
        `Every strategy on ${real.nodes} real nodes and a real Redis, and the simulator fed the same recorded requests. ${real.seeds} seeds each.`,
        real.machine,
        table(
          "Real cluster against the simulator",
          ["Strategy", "Traffic", "Error real", "Error sim", "Calls / 1k real", "Calls / 1k sim"],
          real.rows.map((r) => [
            pretty(r.strategy),
            h("td", {}, r.scenario),
            fmtPct(r.real_error_pct.mean),
            fmtPct(r.sim_error_pct.mean),
            fmtInt(r.real_calls_per_1k.mean),
            fmtInt(r.sim_calls_per_1k.mean),
          ]),
        ),
      ),
    );
  }

  if (overload) root.append(overloadBlock(overload));

  root.append(h("div", { class: "spacer" }));
  return () => {};
}
