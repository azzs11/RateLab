// The algorithm lab: one or two algorithms on the same traffic, every decision and the
// algorithm's state over time, scrubbable.

import { figure } from "../components/figure";
import { mark, surface } from "../lib/canvas";
import { afterPaint, h } from "../lib/dom";
import { fmtInt, fmtMs, pretty } from "../lib/format";
import { player, upperIndex } from "../lib/motion";
import { palette, type Palette } from "../lib/palette";
import { onThemeChange } from "../lib/theme";
import { playerBar } from "../lib/ui";
import { choose, howTo, scrub, sentence } from "../lib/inline";
import { masthead } from "../components/masthead";
import { wasm, type Core } from "../lib/wasm";

const ALGOS = ["token_bucket", "leaky_bucket", "fixed_window", "sliding_window", "gcra"];
const PATTERNS: Record<string, string> = {
  steady: "Steady (Poisson)",
  bursty: "Bursty (on / off)",
  heavy_tailed: "Heavy-tailed (Pareto)",
  diurnal: "Daily ramp",
  flash_crowd: "Flash crowd",
  boundary_attack: "Boundary attack",
};
const SECS = 12;

const HOW: Record<string, string> = {
  token_bucket:
    "Holds up to `limit` tokens and adds `limit` per window, continuously. A request is allowed if a token is there, and takes it. A full bucket lets a burst of `limit` through at once, then the refill rate takes over.",
  leaky_bucket:
    "The same rule seen as water: every request pours one unit into a bucket that drains `limit` per window. A request is refused when it would overflow. It makes exactly the token bucket's decisions.",
  fixed_window:
    "Counts requests in windows aligned to the clock and resets at every edge. Cheap: one counter per key. But a burst just before an edge and another just after both fit, so up to twice the limit can pass in one window-long span.",
  sliding_window:
    "Keeps a log of every admitted request in the last window and allows a new one only if fewer than `limit` are in it. Never lets more than `limit` through in any window-long span, at the cost of memory that grows with the limit.",
  gcra:
    "The Generic Cell Rate Algorithm keeps one timestamp per key: the theoretical arrival time (TAT) of the next request. A request is allowed if TAT − now leaves room for it, and pushes TAT forward by one emission interval. Token bucket decisions, one number of state.",
};

interface Params {
  a: string;
  b: string;
  pattern: string;
  limit: number;
  windowMs: number;
  load: number;
  seed: number;
}

interface Run {
  name: string;
  at: Float64Array;
  allowed: Uint8Array;
  sampleAt: Float64Array;
  sampleRemaining: Uint32Array;
  sampleReset: Float64Array;
  admittedPrefix: Uint32Array;
  admitted: number;
  exact: number;
  worst: number;
}

function simulate(core: Core, p: Params, name: string): Run {
  const traffic = core.Traffic.generate(p.pattern, p.seed, SECS, p.limit, p.windowMs, 1, p.load);
  const run = core.LabRun.run(name, p.limit, p.windowMs, traffic, 0, Math.max(5, p.windowMs / 100));
  const at = Float64Array.from(traffic.atMs());
  const allowed = Uint8Array.from(run.allowed());
  const admittedPrefix = new Uint32Array(at.length + 1);
  for (let i = 0; i < at.length; i++) admittedPrefix[i + 1] = admittedPrefix[i] + allowed[i];
  const r: Run = {
    name,
    at,
    allowed,
    sampleAt: Float64Array.from(run.sampleAtMs()),
    sampleRemaining: Uint32Array.from(run.sampleRemaining()),
    sampleReset: Float64Array.from(run.sampleResetMs()),
    admittedPrefix,
    admitted: run.admitted(),
    exact: run.exactAdmitted(),
    worst: run.worstWindow(),
  };
  run.free();
  traffic.free();
  return r;
}

/** The algorithm's state at time t, drawn as its own picture. */
function drawState(ctx: CanvasRenderingContext2D, w: number, hgt: number, r: Run, p: Params, t: number, c: Palette) {
  const i = Math.max(0, upperIndex(r.sampleAt, t));
  const remaining = r.sampleRemaining[i] ?? p.limit;
  const reset = r.sampleReset[i] ?? 0;
  const used = p.limit - remaining;
  ctx.font = `12px ${c.mono}`;
  ctx.textBaseline = "alphabetic";

  if (r.name === "token_bucket" || r.name === "leaky_bucket") {
    const water = r.name === "token_bucket" ? remaining : used;
    const bw = Math.min(w * 0.42, 170);
    const bh = hgt - 54;
    const x0 = (w - bw) / 2;
    const y0 = 18;
    const level = (water / p.limit) * (bh - 6);
    ctx.fillStyle = c.accent;
    ctx.globalAlpha = 0.85;
    ctx.fillRect(x0 + 3, y0 + bh - 3 - level, bw - 6, level);
    ctx.globalAlpha = 1;
    ctx.strokeStyle = c.ink;
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(x0, y0);
    ctx.lineTo(x0, y0 + bh);
    ctx.lineTo(x0 + bw, y0 + bh);
    ctx.lineTo(x0 + bw, y0);
    ctx.stroke();
    if (r.name === "leaky_bucket") {
      ctx.fillStyle = c.accent;
      ctx.fillRect(x0 + bw / 2 - 2, y0 + bh, 4, 10);
    }
    ctx.lineWidth = 1;
    ctx.fillStyle = c.ink2;
    ctx.textAlign = "center";
    ctx.fillText(
      r.name === "token_bucket" ? `${remaining} of ${p.limit} tokens` : `${used} of ${p.limit} units of water`,
      w / 2,
      hgt - 10,
    );
  } else if (r.name === "fixed_window") {
    const start = Math.floor(t / p.windowMs) * p.windowMs;
    const progress = (t - start) / p.windowMs;
    const bx = 24;
    const bw = w - 48;
    const by = hgt / 2 - 18;
    ctx.fillStyle = c.surface2;
    ctx.fillRect(bx, by, bw, 36);
    ctx.fillStyle = used >= p.limit ? c.blocked : c.accent;
    ctx.fillRect(bx, by, (used / p.limit) * bw, 36);
    ctx.strokeStyle = c.lineStrong;
    ctx.strokeRect(bx, by, bw, 36);
    // Window clock.
    ctx.fillStyle = c.line;
    ctx.fillRect(bx, by + 52, bw, 4);
    ctx.fillStyle = c.ink;
    ctx.fillRect(bx, by + 52, bw * progress, 4);
    ctx.fillStyle = c.ink2;
    ctx.textAlign = "left";
    ctx.fillText(`count ${used} / ${p.limit} in this window`, bx, by - 10);
    ctx.fillText(`window resets in ${fmtMs(start + p.windowMs - t)}`, bx, by + 76);
  } else if (r.name === "sliding_window") {
    const bx = 24;
    const bw = w - 48;
    const y = hgt / 2;
    ctx.strokeStyle = c.lineStrong;
    ctx.beginPath();
    ctx.moveTo(bx, y);
    ctx.lineTo(bx + bw, y);
    ctx.stroke();
    const from = t - p.windowMs;
    let inLog = 0;
    const lo = Math.max(0, upperIndex(r.at, from) + 1);
    const hi = upperIndex(r.at, t);
    for (let k = lo; k <= hi; k++) {
      if (!r.allowed[k]) continue;
      inLog++;
      const x = bx + ((r.at[k] - from) / p.windowMs) * bw;
      ctx.fillStyle = c.accent;
      ctx.fillRect(x - 1.5, y - 16, 3, 32);
    }
    ctx.fillStyle = c.ink2;
    ctx.textAlign = "left";
    ctx.fillText(`log: ${inLog} of ${p.limit} in the last ${fmtMs(p.windowMs)}`, bx, y - 30);
    ctx.fillStyle = c.muted;
    ctx.fillText(`now − ${fmtMs(p.windowMs)}`, bx, y + 34);
    ctx.textAlign = "right";
    ctx.fillText("now", bx + bw, y + 34);
  } else {
    // GCRA: TAT − now on an axis from 0 to one window; allowed while it is below
    // window − emission interval.
    const bx = 24;
    const bw = w - 48;
    const y = hgt / 2;
    const interval = p.windowMs / p.limit;
    const threshold = p.windowMs - interval;
    const x = (v: number) => bx + (Math.min(v, p.windowMs) / p.windowMs) * bw;
    ctx.fillStyle = c.allowedSoft;
    ctx.fillRect(bx, y - 14, x(threshold) - bx, 28);
    ctx.fillStyle = c.blockedSoft;
    ctx.fillRect(x(threshold), y - 14, bx + bw - x(threshold), 28);
    ctx.fillStyle = reset > threshold ? c.blocked : c.accent;
    ctx.fillRect(x(reset) - 2, y - 24, 4, 48);
    ctx.fillStyle = c.ink2;
    ctx.textAlign = "left";
    ctx.fillText(`TAT − now = ${fmtMs(reset)}`, bx, y - 32);
    ctx.fillStyle = c.muted;
    ctx.fillText("0", bx, y + 38);
    ctx.textAlign = "right";
    ctx.fillText(`refuse past ${fmtMs(threshold)}`, bx + bw, y + 38);
  }
  ctx.textAlign = "left";
}

/** Every decision over time, plus the state as an area, with a playhead. */
function drawTimeline(ctx: CanvasRenderingContext2D, w: number, hgt: number, r: Run, p: Params, t: number, c: Palette) {
  const pad = 10;
  const top = 10;
  const areaH = hgt - 64;
  const x = (ms: number) => pad + (ms / (SECS * 1000)) * (w - pad * 2);
  // Window edges.
  ctx.strokeStyle = c.line;
  for (let e = 0; e <= SECS * 1000; e += p.windowMs) {
    if ((SECS * 1000) / p.windowMs > 60) break;
    ctx.beginPath();
    ctx.moveTo(x(e), top);
    ctx.lineTo(x(e), hgt - 18);
    ctx.stroke();
  }
  // Remaining over time.
  const y = (v: number) => top + areaH - (v / p.limit) * areaH;
  ctx.beginPath();
  ctx.moveTo(x(0), y(0));
  for (let i = 0; i < r.sampleAt.length; i++) ctx.lineTo(x(r.sampleAt[i]), y(r.sampleRemaining[i]));
  ctx.lineTo(x(r.sampleAt[r.sampleAt.length - 1] ?? 0), y(0));
  ctx.closePath();
  ctx.fillStyle = c.accentSoft;
  ctx.fill();
  ctx.beginPath();
  for (let i = 0; i < r.sampleAt.length; i++) {
    const xx = x(r.sampleAt[i]);
    const yy = y(r.sampleRemaining[i]);
    if (i === 0) ctx.moveTo(xx, yy);
    else ctx.lineTo(xx, yy);
  }
  ctx.strokeStyle = c.accent;
  ctx.lineWidth = 1.5;
  ctx.stroke();
  ctx.lineWidth = 1;
  // Decisions: allowed on one row, blocked on the row below.
  const rowA = hgt - 40;
  const rowB = hgt - 26;
  for (let i = 0; i < r.at.length; i++) {
    const past = r.at[i] <= t;
    ctx.globalAlpha = past ? 1 : 0.18;
    mark(ctx, x(r.at[i]), r.allowed[i] ? rowA : rowB, 2.6, r.allowed[i] === 1, c.allowed, c.blocked);
  }
  ctx.globalAlpha = 1;
  ctx.font = `11px ${c.mono}`;
  ctx.fillStyle = c.muted;
  for (let s = 0; s <= SECS; s += 2) ctx.fillText(`${s}s`, x(s * 1000) + 2, hgt - 4);
  ctx.strokeStyle = c.ink;
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  ctx.moveTo(x(t), top);
  ctx.lineTo(x(t), hgt - 16);
  ctx.stroke();
  ctx.lineWidth = 1;
}

function panel(r: Run, p: Params, getT: () => number) {
  const stateCanvas = surface(`${pretty(r.name)} state at the playhead`, 3.6, (s) =>
    drawState(s.ctx, s.width, s.height, r, p, getT(), palette()),
    150,
  );
  const timeline = surface(
    `${pretty(r.name)}: ${r.admitted} of ${r.at.length} requests allowed; the exact rule allows ${r.exact}; at most ${r.worst} in any ${p.windowMs} ms.`,
    3.2,
    (s) => drawTimeline(s.ctx, s.width, s.height, r, p, getT(), palette()),
    150,
  );
  const allowedNow = h("span", { class: "value allowed" }, "0");
  const blockedNow = h("span", { class: "value blocked" }, "0");
  const diff = r.admitted - r.exact;
  const el = h(
    "section",
    { class: "lab-panel", "aria-label": pretty(r.name) },
    h("h2", { class: "lab-title" }, pretty(r.name)),
    h("div", { class: "lab-visuals" }, stateCanvas.el, timeline.el),
    h(
      "div",
      { class: "stats" },
      h("div", { class: "stat" }, h("div", { class: "label" }, "Allowed"), allowedNow),
      h("div", { class: "stat" }, h("div", { class: "label" }, "Blocked"), blockedNow),
      h(
        "div",
        { class: "stat" },
        h("div", { class: "label" }, "Peak / window"),
        h("span", { class: `value ${r.worst > p.limit ? "blocked" : ""}` }, `${r.worst}`),
        h("div", { class: "hint" }, `limit ${p.limit}`),
      ),
      h(
        "div",
        { class: "stat" },
        h("div", { class: "label" }, "vs exact"),
        h("span", { class: "value" }, diff === 0 ? "exact" : `${diff > 0 ? "+" : ""}${fmtInt(diff)}`),
        h("div", { class: "hint" }, `exact allows ${fmtInt(r.exact)}`),
      ),
    ),
  );
  return {
    el,
    redraw: () => {
      stateCanvas.redraw();
      timeline.redraw();
      const t = getT();
      const k = upperIndex(r.at, t) + 1;
      allowedNow.textContent = fmtInt(r.admittedPrefix[k]);
      blockedNow.textContent = fmtInt(k - r.admittedPrefix[k]);
    },
    dispose: () => {
      stateCanvas.dispose();
      timeline.dispose();
    },
  };
}

export default async function lab(root: HTMLElement, params: URLSearchParams) {
  root.append(
    masthead(
      "Instrument 1 of 6",
      "Algorithm lab",
      `One algorithm, or two side by side, on the same traffic: every decision over ${SECS} seconds and what the algorithm remembered at each moment.`,
    ),
  );
  await afterPaint();
  const core = await wasm();
  const p: Params = {
    a: ALGOS.includes(params.get("a") ?? "") ? (params.get("a") as string) : "token_bucket",
    b: ALGOS.includes(params.get("b") ?? "") ? (params.get("b") as string) : "",
    pattern: params.get("pattern") && PATTERNS[params.get("pattern")!] ? params.get("pattern")! : "bursty",
    limit: 10,
    windowMs: 1000,
    load: 2,
    seed: 1,
  };

  let panels: ReturnType<typeof panel>[] = [];
  const stage = h("div", { class: "lab-stage" });
  const how = h("div", { class: "lab-how prose" });
  let t = 0;
  const playback = player(
    (time) => {
      t = time;
      bar.update(time);
      panels.forEach((x) => x.redraw());
    },
    { duration: SECS * 1000, onState: (playing) => bar.paint(playing) },
  );
  const bar = playerBar(playback, () => SECS * 1000, [0.25, 1, 4], (s) => playback.setSpeed(s));

  const rebuild = () => {
    panels.forEach((x) => x.dispose());
    const names = [p.a, ...(p.b && p.b !== p.a ? [p.b] : [])];
    panels = names.map((n) => panel(simulate(core, p, n), p, () => t));
    stage.replaceChildren(...panels.map((x) => x.el));
    stage.classList.toggle("two", panels.length === 2);
    how.replaceChildren(
      ...names.flatMap((n) => [h("h2", {}, `How ${pretty(n).toLowerCase().replace("gcra", "GCRA")} decides`), h("p", {}, HOW[n].replace(/`limit`/g, String(p.limit)))]),
    );
    playback.seek(t);
  };

  const algoOptions = ALGOS.map((a) => ({ value: a, text: pretty(a) }));
  const traffic = Object.entries(PATTERNS).map(([value, text]) => ({ value, text: text.toLowerCase() }));
  const fig = figure(
    "1",
    "An algorithm, deciding.",
    "Dots were allowed and crosses refused. The top drawing is the algorithm's state at the playhead; the shaded area below is how much the key had left over time. Computed by the library in your browser.",
    bar.el,
    h("div", { class: "figure-body" }, stage),
  );
  fig.classList.add("wide");

  root.append(
        h(
      "div",
      { class: "page" },
      h(
        "div",
        { class: "prose" },
        sentence(
          choose("Algorithm", algoOptions, p.a, (v) => {
            p.a = v;
            rebuild();
          }).el,
          ", compared with ",
          choose("Compare with", [{ value: "", text: "nothing" }, ...algoOptions], p.b, (v) => {
            p.b = v;
            rebuild();
          }).el,
          ", at a limit of ",
          scrub({ min: 1, max: 50, step: 1, value: p.limit, format: (v) => `${v}`, label: "Limit" }, (v) => {
            p.limit = v;
            rebuild();
          }).el,
          " per ",
          choose(
            "Window",
            [250, 500, 1000, 2000].map((v) => ({ value: String(v), text: fmtMs(v) })),
            String(p.windowMs),
            (v) => {
              p.windowMs = Number(v);
              rebuild();
            },
          ).el,
          ", under ",
          choose("Traffic", traffic, p.pattern, (v) => {
            p.pattern = v;
            rebuild();
          }).el,
          " traffic at ",
          scrub({ min: 0.25, max: 5, step: 0.25, value: p.load, format: (v) => `${v}×`, label: "Load, times the limit" }, (v) => {
            p.load = v;
            rebuild();
          }).el,
          " the limit (seed ",
          scrub({ min: 1, max: 50, step: 1, value: p.seed, format: (v) => `${v}`, label: "Random seed" }, (v) => {
            p.seed = v;
            rebuild();
          }).el,
          ").",
        ),
        howTo(),
        fig,
        how,
      ),
    ),
  );
  rebuild();
  playback.play();
  const stopTheme = onThemeChange(() => panels.forEach((x) => x.redraw()));
  return () => {
    playback.dispose();
    panels.forEach((x) => x.dispose());
    stopTheme();
  };
}
