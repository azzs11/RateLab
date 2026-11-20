// The strategy explorer: the benchmark's Pareto chart, filterable, each point
// replayable in the cluster simulator.

import { figure } from "../components/figure";
import { paretoChart, paretoLegend, type ParetoPoint } from "../components/pareto";
import { loadResults, machineLine, type StrategyResults, type StrategyRow, type Summary } from "../lib/data";
import { h, html } from "../lib/dom";
import { fmtInt, fmtPct, pretty } from "../lib/format";
import { icon } from "../lib/icons";

const ORDER = ["central_exact", "local_split", "async_sync", "fixed_leasing", "adaptive_leasing"];

/** How each benchmark scenario maps onto the cluster simulator's settings. */
const REPLAY: Record<string, Record<string, string>> = {
  steady_poisson: { pattern: "steady", load: "2" },
  skewed: { pattern: "steady", load: "2", hot: "80" },
  on_off: { pattern: "bursty", load: "2" },
  heavy_tailed: { pattern: "heavy_tailed", load: "2" },
  diurnal: { pattern: "diurnal", load: "2" },
  flash_crowd: { pattern: "flash_crowd", load: "1" },
  boundary_attack: { pattern: "boundary_attack", load: "1" },
};

const spread = (s: Summary, d = 2) =>
  s.n > 1 && s.min.toFixed(d) !== s.max.toFixed(d)
    ? `${s.mean.toFixed(d)} (${s.min.toFixed(d)}–${s.max.toFixed(d)})`
    : s.mean.toFixed(d);

function replayLink(row: StrategyRow, data: StrategyResults): string {
  const q = new URLSearchParams({
    strategy: row.strategy,
    nodes: String(Math.max(2, row.nodes)),
    limit: String(data.limit),
    keys: String(data.keys),
    secs: String(Math.min(30, data.size.secs)),
    seed: "1",
    delay: String(data.network.one_way_us),
    jitter: String(data.network.jitter_us),
    ...REPLAY[row.scenario],
  });
  return `#/cluster?${q}`;
}

/** Strategies with fewer-or-equal calls and lower-or-equal error than adaptive leasing. */
function beatsAdaptive(rows: StrategyRow[]) {
  const out: { scenario: string; nodes: number; by: string; byError: number; error: number; byCalls: number; calls: number }[] = [];
  for (const t of rows.filter((r) => r.strategy === "adaptive_leasing")) {
    for (const o of rows.filter((r) => r.strategy !== t.strategy && r.scenario === t.scenario && r.nodes === t.nodes)) {
      const [oc, oe, tc, te] = [o.calls_per_1k.mean, o.error_pct.mean, t.calls_per_1k.mean, t.error_pct.mean];
      if ((oc <= tc && oe < te) || (oc < tc && oe <= te)) {
        out.push({ scenario: t.scenario, nodes: t.nodes, by: o.strategy, byError: oe, error: te, byCalls: oc, calls: tc });
      }
    }
  }
  return out;
}

export default async function explorer(root: HTMLElement, params: URLSearchParams) {
  const data = await loadResults<StrategyResults>("strategies");
  root.append(
    h(
      "div",
      { class: "page view-head" },
      h("p", { class: "kicker" }, "Strategy explorer"),
      h("h1", { class: "display" }, "The price of ", h("span", { class: "hero-em", style: "display:inline" }, "exactness")),
      h(
        "p",
        { class: "lede" },
        "Every point is one coordination strategy on one traffic pattern, averaged over several seeds by the benchmark. Lower left is better: fewer calls to the shared store and less admission error.",
      ),
    ),
  );
  if (!data) {
    root.append(h("div", { class: "page" }, h("p", { class: "error-box" }, "The strategy benchmark has not been run for this build.")));
    return () => {};
  }

  const nodeOptions = data.size.nodes;
  let nodes = Number(params.get("nodes")) || (nodeOptions.includes(3) ? 3 : nodeOptions[0]);
  let scenario = "";
  const [focusStrategy, focusScenario] = (params.get("focus") ?? "").split(":");
  let focus = data.matrix.find((r) => r.nodes === nodes && r.strategy === focusStrategy && r.scenario === focusScenario) ?? null;

  const chartBox = h("div", {});
  const detail = h("div", { class: "explorer-detail", "aria-live": "polite" });
  const tables = h("div", { class: "stack" });
  const scenarios = [...new Set(data.matrix.map((r) => r.scenario))];

  const paintDetail = () => {
    if (!focus) {
      detail.replaceChildren(h("p", { class: "note" }, "Select a point (click it, or tab to it and press Enter) to see its numbers and replay it."));
      return;
    }
    const f = focus;
    detail.replaceChildren(
      h("p", { class: "label" }, `${f.nodes} nodes · ${f.scenario.replace(/_/g, " ")}`),
      h("h2", { class: "display explorer-title" }, pretty(f.strategy)),
      h(
        "dl",
        { class: "explorer-dl" },
        h("div", {}, h("dt", {}, "Admission error"), h("dd", { class: "num" }, `${spread(f.error_pct)}%`)),
        h("div", {}, h("dt", {}, "Over / under"), h("dd", { class: "num" }, `${f.over_pct.mean.toFixed(2)}% / ${f.under_pct.mean.toFixed(2)}%`)),
        h("div", {}, h("dt", {}, "Store calls / 1k"), h("dd", { class: "num" }, spread(f.calls_per_1k, 0))),
        h("div", {}, h("dt", {}, "p99 wait"), h("dd", { class: "num" }, `${spread(f.p99_us, 0)} µs`)),
        h("div", {}, h("dt", {}, "Peak / limit"), h("dd", { class: "num" }, spread(f.worst_window_ratio))),
      ),
      h("a", { class: "btn accent", href: replayLink(f, data) }, "Replay in the cluster simulator ", html(icon.arrow())),
      h(
        "p",
        { class: "note" },
        `The numbers are the benchmark's mean (min–max) over ${data.size.seeds} seeds. The replay is a new run of the same configuration (seed 1) in your browser, so its numbers will be close but not identical.`,
      ),
    );
  };

  const paintChart = () => {
    const points: ParetoPoint[] = data.matrix
      .filter((r) => r.nodes === nodes)
      .map((r) => ({ strategy: r.strategy, scenario: r.scenario, calls: r.calls_per_1k.mean, error: r.error_pct.mean }));
    chartBox.replaceChildren(
      paretoChart(points, {
        title: `${nodes} nodes: store calls against admission error`,
        highlight: scenario || undefined,
        selected: focus ? `${focus.strategy}:${focus.scenario}` : undefined,
        onPick: (p) => {
          focus = data.matrix.find((r) => r.nodes === nodes && r.strategy === p.strategy && r.scenario === p.scenario) ?? null;
          paintDetail();
          paintChart();
          chartBox.querySelector<SVGGElement>('[data-selected="true"]')?.focus();
          history.replaceState(null, "", `#/explorer?nodes=${nodes}&focus=${p.strategy}:${p.scenario}`);
        },
      }),
      paretoLegend(),
    );
  };

  const paintTables = () => {
    const rows = data.matrix.filter((r) => r.nodes === nodes);
    const pivot = (title: string, cell: (r: StrategyRow) => string, best: (r: StrategyRow) => number) =>
      h(
        "div",
        { class: "table-wrap" },
        h(
          "table",
          {},
          h("caption", { class: "table-title" }, title),
          h("thead", {}, h("tr", {}, h("th", { scope: "col" }, "Traffic"), ...ORDER.map((s) => h("th", { class: "n", scope: "col" }, pretty(s))))),
          h(
            "tbody",
            {},
            ...scenarios.map((sc) => {
              const line = ORDER.map((s) => rows.find((r) => r.scenario === sc && r.strategy === s));
              const min = Math.min(...line.filter(Boolean).map((r) => best(r!)));
              return h(
                "tr",
                {},
                h("th", { scope: "row" }, sc.replace(/_/g, " ")),
                ...line.map((r) =>
                  h("td", { class: `n ${r && best(r) === min ? "lead" : ""}` }, r ? cell(r) : "–"),
                ),
              );
            }),
          ),
        ),
      );
    const losses = beatsAdaptive(data.matrix);
    tables.replaceChildren(
      pivot(`Admission error, ${nodes} nodes (%, best in bold)`, (r) => r.error_pct.mean.toFixed(2), (r) => r.error_pct.mean),
      pivot(`Store calls per 1,000 decisions, ${nodes} nodes`, (r) => fmtInt(r.calls_per_1k.mean), (r) => r.calls_per_1k.mean),
      h("h2", { class: "display explorer-sub" }, "Where adaptive leasing loses"),
      h(
        "p",
        {},
        `Configurations, on any cluster size, where another strategy needed no more store calls and made less admission error (or needed fewer calls for the same error). Computed from the same data as RESULTS.md: ${losses.length} of ${data.matrix.filter((r) => r.strategy === "adaptive_leasing").length}.`,
      ),
      h(
        "div",
        { class: "table-wrap" },
        h(
          "table",
          {},
          h(
            "thead",
            {},
            h(
              "tr",
              {},
              ...["Traffic", "Nodes", "Better strategy", "Its error", "Adaptive error", "Its calls / 1k", "Adaptive calls / 1k"].map((x, i) =>
                h("th", { class: i > 0 && i !== 2 ? "n" : "", scope: "col" }, x),
              ),
            ),
          ),
          h(
            "tbody",
            {},
            ...losses.map((l) =>
              h(
                "tr",
                {},
                h("td", {}, l.scenario.replace(/_/g, " ")),
                h("td", { class: "n" }, String(l.nodes)),
                h("td", {}, pretty(l.by)),
                h("td", { class: "n" }, fmtPct(l.byError)),
                h("td", { class: "n" }, fmtPct(l.error)),
                h("td", { class: "n" }, fmtInt(l.byCalls)),
                h("td", { class: "n" }, fmtInt(l.calls)),
              ),
            ),
          ),
        ),
      ),
    );
  };

  const nodeSwitch = h("div", { class: "segmented", role: "group", "aria-label": "Cluster size" });
  for (const n of nodeOptions) {
    const b = h("button", { type: "button", "aria-pressed": String(n === nodes) }, `${n} node${n === 1 ? "" : "s"}`);
    b.addEventListener("click", () => {
      nodes = n;
      nodeSwitch.querySelectorAll("button").forEach((x) => x.setAttribute("aria-pressed", String(x === b)));
      focus = null;
      paintChart();
      paintDetail();
      paintTables();
    });
    nodeSwitch.append(b);
  }
  const scenarioSelect = h("select", { "aria-label": "Highlight a traffic pattern" }) as HTMLSelectElement;
  scenarioSelect.append(h("option", { value: "" }, "All traffic patterns"), ...scenarios.map((s) => h("option", { value: s }, s.replace(/_/g, " "))));
  scenarioSelect.addEventListener("change", () => {
    scenario = scenarioSelect.value;
    paintChart();
  });

  root.append(
    h(
      "div",
      { class: "page" },
      figure(
        "5",
        "Store calls against admission error",
        `From results/strategies.json: ${data.size.secs} s per run, ${data.size.seeds} seeds, limit ${data.limit} per second per key, ${data.keys} keys, simulated one-way delay ${data.network.one_way_us} µs + up to ${data.network.jitter_us} µs. Measured on ${machineLine(data.machine)}.`,
        h("div", { class: "controls explorer-controls" }, nodeSwitch, h("div", { class: "field" }, scenarioSelect)),
        h("div", { class: "figure-body explorer-body" }, chartBox, detail),
      ),
      h("div", { class: "explorer-tables" }, tables),
    ),
  );
  paintChart();
  paintDetail();
  paintTables();
  return () => {};
}
