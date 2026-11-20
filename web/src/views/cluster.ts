// The cluster simulator: N nodes enforcing one limit through a shared store, with any
// coordination strategy, delay, skew and an outage. Runs sim::cluster in the browser.

import { figure } from "../components/figure";
import { mark, surface } from "../lib/canvas";
import { h } from "../lib/dom";
import { fmtInt, fmtMs, fmtPct, pretty } from "../lib/format";
import { player, upperIndex } from "../lib/motion";
import { palette, STRATEGY_STYLE } from "../lib/palette";
import { onThemeChange } from "../lib/theme";
import { checkField, playerBar, rangeField, selectField } from "../lib/ui";
import { wasm, type Core } from "../lib/wasm";

const STRATEGIES = ["central_exact", "local_split", "async_sync", "fixed_leasing", "adaptive_leasing"];
const PATTERNS: Record<string, string> = {
  steady: "Steady (Poisson)",
  bursty: "Bursty (on / off)",
  heavy_tailed: "Heavy-tailed (Pareto)",
  diurnal: "Daily ramp",
  flash_crowd: "Flash crowd",
  boundary_attack: "Boundary attack",
};
const ABOUT: Record<string, string> = {
  central_exact: "Every request asks the store. Exact, and every request waits one round trip.",
  local_split: "Each node gets limit ÷ N and never talks to the store. Free, and wrong when traffic is uneven.",
  async_sync: "Nodes decide locally and report their counts every 10 ms. No waiting; nodes over-admit between syncs.",
  fixed_leasing: "Nodes borrow fixed chunks of tokens from the store and spend them locally. Leftovers expire.",
  adaptive_leasing: "Leases grow when used up quickly, shrink when the store runs low, and idle leftovers go back.",
};
/** Messages are drawn travelling this long (simulated ms), far slower than they really do. */
const VISUAL_TRAVEL_MS = 60;
const BIN_MS = 100;

interface Params {
  strategy: string;
  nodes: number;
  pattern: string;
  limit: number;
  load: number;
  keys: number;
  hot: number;
  delayUs: number;
  jitterUs: number;
  skewMs: number;
  outage: boolean;
  failClosed: boolean;
  seed: number;
  secs: number;
}

interface Result {
  at: Float64Array;
  node: Uint8Array;
  allowed: Uint8Array;
  ideal: Uint8Array;
  wait: Float64Array;
  sampleAt: Float64Array;
  held: Uint32Array;
  msgAt: Float64Array;
  msgNode: Uint8Array;
  msgKind: Uint8Array;
  msgLost: Uint8Array;
  admittedPrefix: Uint32Array;
  idealPrefix: Uint32Array;
  binsGot: Uint32Array;
  binsWant: Uint32Array;
  over: number;
  under: number;
  calls: number;
  p50: number;
  p99: number;
  outageFrom: number;
  outageTo: number;
}

function run(core: Core, p: Params): Result {
  const traffic = core.Traffic.generate(p.pattern, p.seed, p.secs, p.limit, 1000, p.keys, p.load);
  const o = new core.ClusterOptions();
  o.strategy = p.strategy;
  o.nodes = p.nodes;
  o.limit = p.limit;
  o.window_ms = 1000;
  o.delay_us = p.delayUs;
  o.jitter_us = p.jitterUs;
  o.skew_ms = p.skewMs;
  o.fail_closed = p.failClosed;
  o.hot_pct = p.hot;
  o.seed = p.seed;
  o.sample_ms = 25;
  const outageFrom = p.outage ? p.secs * 1000 * 0.4 : 0;
  const outageTo = p.outage ? p.secs * 1000 * 0.6 : 0;
  o.outage_from_ms = outageFrom;
  o.outage_to_ms = outageTo;
  const v = core.ClusterView.run(o, traffic);
  const at = Float64Array.from(v.atMs());
  const allowed = Uint8Array.from(v.allowed());
  const ideal = Uint8Array.from(v.ideal());
  const admittedPrefix = new Uint32Array(at.length + 1);
  const idealPrefix = new Uint32Array(at.length + 1);
  const bins = Math.ceil((p.secs * 1000) / BIN_MS) + 1;
  const binsGot = new Uint32Array(bins);
  const binsWant = new Uint32Array(bins);
  for (let i = 0; i < at.length; i++) {
    admittedPrefix[i + 1] = admittedPrefix[i] + allowed[i];
    idealPrefix[i + 1] = idealPrefix[i] + ideal[i];
    const b = Math.min(bins - 1, Math.floor(at[i] / BIN_MS));
    binsGot[b] += allowed[i];
    binsWant[b] += ideal[i];
  }
  const r: Result = {
    at,
    node: Uint8Array.from(v.node()),
    allowed,
    ideal,
    wait: Float64Array.from(v.waitUs()),
    sampleAt: Float64Array.from(v.sampleAtMs()),
    held: Uint32Array.from(v.held()),
    msgAt: Float64Array.from(v.msgAtMs()),
    msgNode: Uint8Array.from(v.msgNode()),
    msgKind: Uint8Array.from(v.msgKind()),
    msgLost: Uint8Array.from(v.msgLost()),
    admittedPrefix,
    idealPrefix,
    binsGot,
    binsWant,
    over: v.overPct(),
    under: v.underPct(),
    calls: v.callsPer1k(),
    p50: v.p50Us(),
    p99: v.p99Us(),
    outageFrom,
    outageTo,
  };
  v.free();
  o.free();
  traffic.free();
  return r;
}

function nodePositions(n: number, w: number, hgt: number): { x: number; y: number }[] {
  const out: { x: number; y: number }[] = [];
  if (w < 600) {
    // Narrow screens: one straight row under the store.
    for (let i = 0; i < n; i++) out.push({ x: (w * (i + 0.5)) / n, y: hgt * 0.62 });
    return out;
  }
  const cx = w / 2;
  const cy = hgt * 0.36;
  const rx = Math.min(w * 0.44, 420);
  const ry = hgt * 0.42;
  for (let i = 0; i < n; i++) {
    // Spread along the lower half of an ellipse around the store.
    const a = Math.PI * (0.12 + (0.76 * (i + 0.5)) / n);
    out.push({ x: cx - Math.cos(a) * rx, y: cy + Math.sin(a) * ry });
  }
  return out;
}

function drawCluster(ctx: CanvasRenderingContext2D, w: number, hgt: number, r: Result, p: Params, t: number) {
  const c = palette();
  const store = { x: w / 2, y: hgt * 0.16 };
  const nodes = nodePositions(p.nodes, w, hgt);
  const down = p.outage && t >= r.outageFrom && t < r.outageTo;

  // Links.
  ctx.lineWidth = 1;
  for (const n of nodes) {
    ctx.strokeStyle = c.line;
    ctx.setLineDash(down ? [4, 5] : []);
    ctx.beginPath();
    ctx.moveTo(n.x, n.y - 18);
    ctx.lineTo(store.x, store.y + 22);
    ctx.stroke();
  }
  ctx.setLineDash([]);

  // Messages in flight (drawn slowed down).
  const from = upperIndex(r.msgAt, t - VISUAL_TRAVEL_MS) + 1;
  const to = upperIndex(r.msgAt, t);
  const style = STRATEGY_STYLE[p.strategy];
  for (let i = from; i <= to; i++) {
    const n = nodes[r.msgNode[i]];
    if (!n) continue;
    const f = (t - r.msgAt[i]) / VISUAL_TRAVEL_MS;
    const lost = r.msgLost[i] === 1;
    if (lost && f > 0.7) continue;
    const x = n.x + (store.x - n.x) * f;
    const y = n.y - 18 + (store.y + 22 - (n.y - 18)) * f;
    ctx.fillStyle = lost ? c.blocked : style?.colour ?? c.accent;
    ctx.beginPath();
    ctx.arc(x, y, 3, 0, Math.PI * 2);
    ctx.fill();
  }

  // Store.
  ctx.fillStyle = down ? c.blockedSoft : c.surface;
  ctx.strokeStyle = down ? c.blocked : c.ink;
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  ctx.roundRect(store.x - 70, store.y - 22, 140, 44, 10);
  ctx.fill();
  ctx.stroke();
  ctx.lineWidth = 1;
  ctx.fillStyle = down ? c.blocked : c.ink;
  ctx.font = `600 13px ${c.text}`;
  ctx.textAlign = "center";
  ctx.fillText(down ? "Store unreachable" : "Shared store", store.x, store.y + 5);

  // Nodes with held tokens and recent requests.
  const si = Math.max(0, upperIndex(r.sampleAt, t));
  const recentFrom = upperIndex(r.at, t - 250) + 1;
  const recentTo = upperIndex(r.at, t);
  // Node boxes shrink on narrow screens so neighbours never overlap.
  const boxW = Math.max(40, Math.min(92, (w < 600 ? w : w * 0.85) / p.nodes - 6));
  const half = boxW / 2;
  nodes.forEach((n, i) => {
    const held = r.held[si * p.nodes + i] ?? 0;
    ctx.fillStyle = c.surface;
    ctx.strokeStyle = c.lineStrong;
    ctx.beginPath();
    ctx.roundRect(n.x - half, n.y - 18, boxW, 54, 8);
    ctx.fill();
    ctx.stroke();
    ctx.fillStyle = c.ink;
    ctx.font = `600 12px ${c.text}`;
    ctx.fillText(boxW < 70 ? `${i + 1}` : `Node ${i + 1}`, n.x, n.y - 2);
    // Held tokens bar (relative to the limit).
    const frac = Math.min(1, held / Math.max(1, p.limit));
    ctx.fillStyle = c.surface2;
    ctx.fillRect(n.x - half + 8, n.y + 8, boxW - 16, 8);
    ctx.fillStyle = c.accent;
    ctx.fillRect(n.x - half + 8, n.y + 8, (boxW - 16) * frac, 8);
    ctx.fillStyle = c.muted;
    ctx.font = `10px ${c.mono}`;
    ctx.fillText(boxW < 70 ? `${held}` : `holds ${held}`, n.x, n.y + 30);
  });
  // Recent requests fall into their node.
  for (let k = recentFrom; k <= recentTo; k++) {
    const n = nodes[r.node[k]];
    if (!n) continue;
    const age = (t - r.at[k]) / 250;
    const jitter = ((k * 37) % 60) - 30;
    ctx.globalAlpha = 1 - age;
    mark(ctx, n.x + jitter, n.y + 50 + age * 40, 3, r.allowed[k] === 1, c.allowed, c.blocked);
  }
  ctx.globalAlpha = 1;
  ctx.textAlign = "left";
}

function drawBins(ctx: CanvasRenderingContext2D, w: number, hgt: number, r: Result, p: Params, t: number) {
  const c = palette();
  const pad = 8;
  const n = r.binsGot.length;
  const max = Math.max(1, ...r.binsWant, ...r.binsGot);
  const x = (b: number) => pad + (b / (n - 1)) * (w - pad * 2);
  const y = (v: number) => hgt - 22 - (v / max) * (hgt - 34);
  if (p.outage) {
    ctx.fillStyle = c.blockedSoft;
    ctx.fillRect(x(r.outageFrom / BIN_MS), 4, x(r.outageTo / BIN_MS) - x(r.outageFrom / BIN_MS), hgt - 26);
  }
  const line = (bins: Uint32Array, colour: string, dash: number[]) => {
    ctx.strokeStyle = colour;
    ctx.setLineDash(dash);
    ctx.lineWidth = 1.6;
    ctx.beginPath();
    for (let b = 0; b < n; b++) {
      if (b === 0) ctx.moveTo(x(b), y(bins[b]));
      else ctx.lineTo(x(b), y(bins[b]));
    }
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.lineWidth = 1;
  };
  line(r.binsWant, c.muted, [5, 4]);
  line(r.binsGot, c.accent, []);
  ctx.strokeStyle = c.ink;
  ctx.beginPath();
  ctx.moveTo(x(t / BIN_MS), 4);
  ctx.lineTo(x(t / BIN_MS), hgt - 20);
  ctx.stroke();
  ctx.fillStyle = c.muted;
  ctx.font = `11px ${c.mono}`;
  for (let s = 0; s <= p.secs; s += 2) ctx.fillText(`${s}s`, x((s * 1000) / BIN_MS) + 2, hgt - 5);
}

function readParams(q: URLSearchParams): Params {
  const num = (k: string, d: number, lo: number, hi: number) => {
    const v = Number(q.get(k));
    return q.has(k) && Number.isFinite(v) ? Math.min(hi, Math.max(lo, v)) : d;
  };
  const strategy = STRATEGIES.includes(q.get("strategy") ?? "") ? q.get("strategy")! : "adaptive_leasing";
  const pattern = PATTERNS[q.get("pattern") ?? ""] ? q.get("pattern")! : "steady";
  return {
    strategy,
    nodes: num("nodes", 3, 2, 8),
    pattern,
    limit: num("limit", 100, 5, 500),
    load: num("load", 2, 0.25, 5),
    keys: num("keys", 1, 1, 20),
    hot: num("hot", 0, 0, 95),
    delayUs: num("delay", 250, 0, 20000),
    jitterUs: num("jitter", 250, 0, 20000),
    skewMs: num("skew", 0, 0, 200),
    outage: q.get("outage") === "1",
    failClosed: q.get("closed") === "1",
    seed: num("seed", 1, 1, 50),
    secs: num("secs", 10, 2, 30),
  };
}

export default async function cluster(root: HTMLElement, query: URLSearchParams) {
  const core = await wasm();
  const p = readParams(query);
  let r = run(core, p);
  let t = 0;

  const meter = {
    admitted: h("span", { class: "value" }, "0"),
    ideal: h("span", { class: "value" }, "0"),
    diff: h("span", { class: "value" }, "0"),
    error: h("span", { class: "value" }, "–"),
    calls: h("span", { class: "value" }, "–"),
    wait: h("span", { class: "value" }, "–"),
  };
  const about = h("p", { class: "note cluster-about" });

  const diagram = surface("Cluster diagram", 1.9, (s) => drawCluster(s.ctx, s.width, s.height, r, p, t), 320);
  const bins = surface(
    "Requests admitted per 100 ms by the cluster, against what one perfectly coordinated limiter would admit",
    5.5,
    (s) => drawBins(s.ctx, s.width, s.height, r, p, t),
    120,
  );

  const paintTotals = () => {
    meter.error.textContent = fmtPct(r.over + r.under);
    meter.error.title = `over ${fmtPct(r.over)}, under ${fmtPct(r.under)}`;
    meter.calls.textContent = fmtInt(r.calls);
    meter.wait.textContent = r.p99 < 1 ? "0" : fmtMs(r.p99 / 1000);
    about.textContent = ABOUT[p.strategy];
    diagram.surface.canvas.setAttribute(
      "aria-label",
      `${p.nodes} nodes running ${pretty(p.strategy)}. Admission error ${fmtPct(r.over + r.under)}, ${fmtInt(r.calls)} store calls per 1,000 decisions, p99 wait ${meter.wait.textContent}.`,
    );
  };
  const frame = (time: number) => {
    t = time;
    bar.update(time);
    diagram.redraw();
    bins.redraw();
    const k = upperIndex(r.at, t) + 1;
    const got = r.admittedPrefix[k];
    const want = r.idealPrefix[k];
    meter.admitted.textContent = fmtInt(got);
    meter.ideal.textContent = fmtInt(want);
    const d = got - want;
    meter.diff.textContent = d === 0 ? "0" : `${d > 0 ? "+" : ""}${fmtInt(d)}`;
    meter.diff.className = `value ${d > 0 ? "blocked" : ""}`;
  };
  const playback = player(frame, { duration: p.secs * 1000, speed: 0.25, onState: (playing) => bar.paint(playing) });
  const bar = playerBar(playback, () => p.secs * 1000, [0.1, 0.25, 1], (s) => playback.setSpeed(s));
  // The player starts at 0.25×; mark that button.
  bar.el.querySelectorAll(".segmented button").forEach((b) => b.setAttribute("aria-pressed", String(b.textContent === "0.25×")));

  const rerun = () => {
    r = run(core, p);
    playback.setDuration(p.secs * 1000);
    paintTotals();
    frame(Math.min(t, p.secs * 1000));
    const q = new URLSearchParams({
      strategy: p.strategy,
      nodes: String(p.nodes),
      pattern: p.pattern,
      limit: String(p.limit),
      load: String(p.load),
      keys: String(p.keys),
      hot: String(p.hot),
      delay: String(p.delayUs),
      jitter: String(p.jitterUs),
      skew: String(p.skewMs),
      outage: p.outage ? "1" : "0",
      closed: p.failClosed ? "1" : "0",
      seed: String(p.seed),
      secs: String(p.secs),
    });
    history.replaceState(null, "", `#/cluster?${q}`);
  };

  const controls = h(
    "div",
    { class: "controls" },
    selectField("Strategy", STRATEGIES.map((s) => ({ value: s, text: pretty(s) })), p.strategy, (v) => {
      p.strategy = v;
      rerun();
    }).el,
    rangeField("Nodes", { min: 2, max: 8, step: 1, value: p.nodes, format: (v) => `${v}` }, (v) => {
      p.nodes = v;
      rerun();
    }).el,
    selectField("Traffic", Object.entries(PATTERNS).map(([value, text]) => ({ value, text })), p.pattern, (v) => {
      p.pattern = v;
      rerun();
    }).el,
    rangeField("Load", { min: 0.25, max: 5, step: 0.25, value: p.load, format: (v) => `${v}× the limit` }, (v) => {
      p.load = v;
      rerun();
    }).el,
    rangeField("Hot node", { min: 0, max: 95, step: 5, value: p.hot, format: (v) => (v === 0 ? "even spread" : `${v}% on node 1`) }, (v) => {
      p.hot = v;
      rerun();
    }).el,
    rangeField("One-way delay", { min: 0, max: 5000, step: 50, value: p.delayUs, format: (v) => fmtMs(v / 1000) }, (v) => {
      p.delayUs = v;
      rerun();
    }).el,
    rangeField("Clock skew", { min: 0, max: 100, step: 5, value: p.skewMs, format: (v) => (v === 0 ? "none" : `${v} ms per node`) }, (v) => {
      p.skewMs = v;
      rerun();
    }).el,
    h(
      "div",
      { class: "field" },
      h("span", {}, "Failures"),
      checkField("Store outage at 40–60%", p.outage, (v) => {
        p.outage = v;
        rerun();
      }),
      checkField("Fail closed", p.failClosed, (v) => {
        p.failClosed = v;
        rerun();
      }),
    ),
  );

  root.append(
    h(
      "div",
      { class: "page view-head" },
      h("p", { class: "kicker" }, "Cluster simulator"),
      h("h1", { class: "display" }, "One limit, ", h("span", { class: "hero-em", style: "display:inline" }, "many nodes")),
      h(
        "p",
        { class: "lede" },
        "Choose how the nodes coordinate, then break things: skew their clocks, cut the store off, pile the traffic onto one node. The meter compares the cluster with one perfectly coordinated limiter.",
      ),
    ),
    h(
      "div",
      { class: "page" },
      figure(
        "4",
        "A cluster, coordinating",
        `Runs the project's deterministic simulator in your browser. Message pulses are drawn slowed down (${VISUAL_TRAVEL_MS} ms of simulated time per trip); the real one-way delay is the setting above. Bars show the tokens each node holds.`,
        controls,
        bar.el,
        h(
          "div",
          { class: "figure-body cluster-body" },
          h("div", { class: "cluster-diagram" }, diagram.el, about),
          h(
            "div",
            { class: "cluster-meter", "aria-label": "Live meter" },
            h("div", { class: "stat" }, h("div", { class: "label" }, "Admitted"), meter.admitted),
            h("div", { class: "stat" }, h("div", { class: "label" }, "Ideal"), meter.ideal),
            h("div", { class: "stat" }, h("div", { class: "label" }, "Difference"), meter.diff),
            h("div", { class: "stat" }, h("div", { class: "label" }, "Error"), meter.error, h("div", { class: "hint" }, "whole run")),
            h("div", { class: "stat" }, h("div", { class: "label" }, "Calls / 1k"), meter.calls),
            h("div", { class: "stat" }, h("div", { class: "label" }, "p99 wait"), meter.wait),
          ),
        ),
        h(
          "div",
          { class: "figure-body bins" },
          h("p", { class: "label" }, "Admitted per 100 ms: cluster (solid) vs ideal (dashed)"),
          bins.el,
        ),
      ),
      h("div", { class: "spacer" }),
    ),
  );
  paintTotals();
  frame(0);
  playback.play();
  const stopTheme = onThemeChange(() => frame(t));
  return () => {
    playback.dispose();
    diagram.dispose();
    bins.dispose();
    stopTheme();
  };
}
