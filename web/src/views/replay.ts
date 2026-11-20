// Bring your own logs: replay real traffic through every algorithm and strategy, and get
// a recommended limit. The log is parsed and replayed in this page; it never leaves it.

import { figure } from "../components/figure";
import { h, html } from "../lib/dom";
import { fmtInt, fmtMs, fmtPct, pretty } from "../lib/format";
import { icon } from "../lib/icons";
import { guessColumns, readTable, toArrays, type Parsed, type Table } from "../lib/logs";
import { wasm, type Core } from "../lib/wasm";
import type { ReplayReport } from "../wasm-pkg/wasm.js";

const WINDOWS = [
  { ms: 1000, text: "1 second" },
  { ms: 10_000, text: "10 seconds" },
  { ms: 60_000, text: "1 minute" },
  { ms: 3_600_000, text: "1 hour" },
];

/** A synthetic access log, generated here so visitors can try the page without a file. */
function sampleLog(core: Core): string {
  const t = core.Traffic.generate("heavy_tailed", 11, 120, 5, 1000, 40, 1.2);
  const at = t.atMs();
  const keys = t.keys();
  t.free();
  const start = Date.UTC(2026, 0, 15, 9, 0, 0);
  const lines = ["timestamp,api_key,path,status"];
  const paths = ["/v1/search", "/v1/items", "/v1/orders", "/v1/me"];
  for (let i = 0; i < at.length; i++) {
    // Key 0 and 1 are made busier, like a couple of heavy integrations.
    const k = keys[i] % 7 === 0 ? 0 : keys[i] % 11 === 0 ? 1 : keys[i];
    lines.push(`${new Date(start + at[i]).toISOString()},key_${String(k).padStart(3, "0")},${paths[i % 4]},200`);
  }
  return lines.join("\n");
}

export default async function replay(root: HTMLElement) {
  const core = await wasm();
  let table: Table | null = null;
  let parsed: Parsed | null = null;
  let windowMs = 1000;
  let target = 1;
  let nodes = 3;

  const status = h("p", { class: "note", role: "status", "aria-live": "polite" });
  const mapping = h("div", { class: "replay-mapping" });
  const results = h("div", { class: "replay-results", "aria-live": "polite" });

  const fileInput = h("input", { type: "file", id: "log-file", accept: ".csv,.tsv,.log,.txt,.jsonl,.json,text/*", class: "visually-hidden" }) as HTMLInputElement;
  const drop = h(
    "label",
    { class: "drop", for: "log-file" },
    html(icon.upload()),
    h("span", { class: "drop-title" }, "Drop an access log here, or choose a file"),
    h("span", { class: "note" }, "CSV, JSON Lines or nginx / Apache combined format, up to 500,000 rows. Parsed in this page; nothing is uploaded."),
  );
  const sampleBtn = h("button", { class: "btn ghost", type: "button" }, "Try a generated sample log");

  const load = (text: string, name: string) => {
    try {
      table = readTable(text);
      const g = guessColumns(table);
      paintMapping(g.time, g.key, g.cost, name);
    } catch (e) {
      status.textContent = String(e instanceof Error ? e.message : e);
      table = null;
    }
  };

  fileInput.addEventListener("change", async () => {
    const f = fileInput.files?.[0];
    if (f) load(await f.text(), f.name);
  });
  drop.addEventListener("dragover", (e) => {
    e.preventDefault();
    drop.classList.add("over");
  });
  drop.addEventListener("dragleave", () => drop.classList.remove("over"));
  drop.addEventListener("drop", async (e) => {
    e.preventDefault();
    drop.classList.remove("over");
    const f = e.dataTransfer?.files?.[0];
    if (f) load(await f.text(), f.name);
  });
  sampleBtn.addEventListener("click", () => load(sampleLog(core), "generated sample"));

  function columnSelect(label: string, value: number, optional: boolean, onChange: (v: number) => void) {
    const id = `col-${label.replace(/\W/g, "")}`;
    const sel = h("select", { id }) as HTMLSelectElement;
    if (optional) sel.append(h("option", { value: "-1" }, "None (cost 1)"));
    table!.columns.forEach((c, i) => {
      const o = h("option", { value: String(i) }, c);
      if (i === value) o.selected = true;
      sel.append(o);
    });
    if (value < 0 && optional) sel.value = "-1";
    sel.addEventListener("change", () => onChange(Number(sel.value)));
    return h("div", { class: "field" }, h("label", { for: id }, label), sel);
  }

  function paintMapping(time: number, key: number, cost: number, name: string) {
    if (!table) return;
    const t = table;
    const cols = { time, key, cost };
    const preview = h(
      "div",
      { class: "table-wrap" },
      h(
        "table",
        {},
        h("caption", { class: "table-title" }, `First rows of ${name} (${t.format}, ${fmtInt(t.rows.length)} rows)`),
        h("thead", {}, h("tr", {}, ...t.columns.map((c) => h("th", { scope: "col" }, c)))),
        h("tbody", {}, ...t.rows.slice(0, 4).map((r) => h("tr", {}, ...t.columns.map((_, i) => h("td", {}, (r[i] ?? "").slice(0, 40)))))),
      ),
    );
    const go = h("button", { class: "btn accent", type: "button" }, "Replay this log ", html(icon.arrow()));
    go.addEventListener("click", () => {
      parsed = toArrays(t, cols.time, cols.key, cols.cost);
      if (parsed.atMs.length === 0) {
        status.textContent = "No row had a time that could be read. Pick another time column.";
        return;
      }
      status.textContent = `Read ${fmtInt(parsed.atMs.length)} requests from ${fmtInt(parsed.keyNames.length)} keys${parsed.skipped ? `; skipped ${fmtInt(parsed.skipped)} rows without a readable time` : ""}.`;
      analyse(true);
    });
    mapping.replaceChildren(
      h("h2", { class: "display replay-step" }, "Which column is which?"),
      h(
        "div",
        { class: "controls replay-cols" },
        columnSelect("Time", time, false, (v) => (cols.time = v)),
        columnSelect("Key (who is limited)", key, false, (v) => (cols.key = v)),
        columnSelect("Cost", cost, true, (v) => (cols.cost = v)),
      ),
      preview,
      h("div", { class: "row" }, go),
    );
    results.replaceChildren();
    mapping.querySelector("select")?.focus();
  }

  function analyse(recommend: boolean) {
    if (!parsed) return;
    results.replaceChildren(h("p", { class: "loading" }, "Replaying…"));
    // Let the message paint before the work starts.
    setTimeout(() => {
      const p = parsed!;
      const traffic = core.Traffic.fromArrays(p.atMs, p.keys, p.costs);
      try {
        if (recommend) limitInput.value = String(core.recommendLimit(traffic, windowMs, target));
        const limit = Math.max(1, Math.round(Number(limitInput.value) || 1));
        const rep = core.ReplayReport.run(traffic, limit, windowMs, nodes);
        paintResults(rep, limit);
        rep.free();
      } catch (e) {
        results.replaceChildren(h("p", { class: "error-box", role: "alert" }, String(e)));
      } finally {
        traffic.free();
      }
    }, 30);
  }

  const limitInput = h("input", { type: "number", id: "limit", min: 1, step: 1, value: 100 }) as HTMLInputElement;
  const settings = h(
    "div",
    { class: "controls" },
    h(
      "div",
      { class: "field" },
      h("label", { for: "win" }, "Window"),
      (() => {
        const s = h("select", { id: "win" }) as HTMLSelectElement;
        WINDOWS.forEach((w) => s.append(h("option", { value: String(w.ms) }, w.text)));
        s.addEventListener("change", () => {
          windowMs = Number(s.value);
          analyse(true);
        });
        return s;
      })(),
    ),
    h(
      "div",
      { class: "field" },
      h("label", { for: "target" }, "Block at most"),
      (() => {
        const s = h("select", { id: "target" }) as HTMLSelectElement;
        [0, 0.1, 1, 5].forEach((v) => {
          const o = h("option", { value: String(v) }, `${v}% of requests`);
          if (v === target) o.selected = true;
          s.append(o);
        });
        s.addEventListener("change", () => {
          target = Number(s.value);
          analyse(true);
        });
        return s;
      })(),
    ),
    h("div", { class: "field" }, h("label", { for: "limit" }, "Limit per key per window"), limitInput),
    h(
      "div",
      { class: "field" },
      h("label", { for: "nodes" }, "Nodes for strategies"),
      (() => {
        const s = h("select", { id: "nodes" }) as HTMLSelectElement;
        [2, 3, 5, 8].forEach((v) => {
          const o = h("option", { value: String(v) }, `${v} nodes`);
          if (v === nodes) o.selected = true;
          s.append(o);
        });
        s.addEventListener("change", () => {
          nodes = Number(s.value);
          analyse(false);
        });
        return s;
      })(),
    ),
  );
  limitInput.addEventListener("change", () => analyse(false));

  function paintResults(rep: ReplayReport, limit: number) {
    const win = WINDOWS.find((w) => w.ms === windowMs)?.text ?? fmtMs(windowMs);
    const algoRows = rep.algorithms.map((name: string, i: number) =>
      h(
        "tr",
        {},
        h("th", { scope: "row" }, pretty(name)),
        h("td", { class: "n" }, fmtPct(rep.algorithm_blocked_pct[i])),
        h("td", { class: "n" }, fmtInt(rep.algorithm_keys_limited[i])),
        h("td", { class: `n ${rep.algorithm_worst_window[i] > limit ? "over" : ""}` }, `${fmtInt(rep.algorithm_worst_window[i])}`),
      ),
    );
    const stratRows = rep.strategies.map((name: string, i: number) =>
      h(
        "tr",
        {},
        h("th", { scope: "row" }, pretty(name)),
        h("td", { class: "n" }, fmtPct(rep.strategy_error_pct[i])),
        h("td", { class: "n" }, fmtInt(rep.strategy_calls_per_1k[i])),
        h("td", { class: "n" }, rep.strategy_p99_us[i] < 1 ? "0" : `${fmtInt(rep.strategy_p99_us[i])} µs`),
      ),
    );
    results.replaceChildren(
      h(
        "div",
        { class: "callout replay-reco" },
        h("p", { class: "label" }, "Recommendation"),
        h(
          "p",
          { class: "replay-big" },
          h("span", { class: "num" }, fmtInt(limit)),
          ` requests per key per ${win}`,
        ),
        h(
          "p",
          {},
          `The smallest limit at which the exact rule (sliding window log) blocks at most ${target}% of this log's requests. Per-key peaks in one ${win}: median ${fmtInt(rep.peak_p50)}, 90th percentile ${fmtInt(rep.peak_p90)}, 99th ${fmtInt(rep.peak_p99)}, busiest key ${fmtInt(rep.peak_max)}.`,
        ),
      ),
      h(
        "div",
        { class: "stats replay-stats" },
        h("div", { class: "stat" }, h("div", { class: "label" }, "Requests"), h("div", { class: "value" }, fmtInt(rep.requests))),
        h("div", { class: "stat" }, h("div", { class: "label" }, "Keys"), h("div", { class: "value" }, fmtInt(rep.keys))),
        h("div", { class: "stat" }, h("div", { class: "label" }, "Time span"), h("div", { class: "value" }, fmtMs(rep.duration_ms))),
      ),
      h(
        "div",
        { class: "grid-2" },
        h(
          "div",
          { class: "table-wrap" },
          h(
            "table",
            {},
            h("caption", { class: "table-title" }, `Each algorithm at ${fmtInt(limit)} per ${win}`),
            h("thead", {}, h("tr", {}, h("th", { scope: "col" }, "Algorithm"), h("th", { class: "n", scope: "col" }, "Blocked"), h("th", { class: "n", scope: "col" }, "Keys limited"), h("th", { class: "n", scope: "col" }, "Peak / window"))),
            h("tbody", {}, ...algoRows),
          ),
        ),
        h(
          "div",
          { class: "table-wrap" },
          h(
            "table",
            {},
            h("caption", { class: "table-title" }, `Sharing the limit across ${nodes} nodes`),
            h("thead", {}, h("tr", {}, h("th", { scope: "col" }, "Strategy"), h("th", { class: "n", scope: "col" }, "Error"), h("th", { class: "n", scope: "col" }, "Calls / 1k"), h("th", { class: "n", scope: "col" }, "p99 wait"))),
            h("tbody", {}, ...stratRows),
          ),
        ),
      ),
      h(
        "p",
        { class: "note" },
        "Peak / window above the limit means that algorithm let more through in some window-long span than the limit: fixed windows do this at edges, and the buckets and GCRA allow a full burst on top of a window of refill (up to about twice the limit). Strategy error is measured against one perfectly coordinated token bucket, with requests spread evenly over the nodes and a simulated 250 µs one-way delay.",
      ),
    );
  }

  root.append(
    h(
      "div",
      { class: "page view-head" },
      h("p", { class: "kicker" }, "Replay"),
      h("h1", { class: "display" }, "Your traffic, ", h("span", { class: "hero-em", style: "display:inline" }, "every algorithm")),
      h(
        "p",
        { class: "lede" },
        "Choosing a limit by guessing is how rate limits end up blocking good customers. Load an access log and see what each algorithm would have done with it, and which limit keeps blocking under your target.",
      ),
    ),
    h(
      "div",
      { class: "page replay-page" },
      figure(
        "6",
        "A log, replayed",
        "Your file is read and replayed by WebAssembly inside this page. Nothing is sent to a server.",
        h("div", { class: "figure-body stack" }, drop, fileInput, h("div", { class: "row" }, sampleBtn), status),
        h("div", { class: "figure-body" }, mapping),
        settings,
        h("div", { class: "figure-body" }, results),
      ),
    ),
  );
  return () => {};
}
