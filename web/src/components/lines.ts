// A plain line chart in SVG: zero-based axes, hairline grid, lines that differ by dash
// as well as colour, and a legend underneath. The numbers live in tables nearby; the
// chart shows their shape.

import { h } from "../lib/dom";

export interface Series {
  name: string;
  colour: string;
  dash?: string;
  width?: number;
  points: [number, number][];
  /** Draw as steps (for counts that change at instants, like workers). */
  step?: boolean;
}

const fmtTick = (v: number) =>
  Math.abs(v) >= 1000 ? `${(v / 1000).toFixed(v % 1000 === 0 ? 0 : 1)}k` : `${Math.round(v)}`;

export function lineChart(
  series: Series[],
  opts: { title: string; xLabel: string; yLabel: string; height?: number; xMax?: number },
): HTMLElement {
  // Narrow screens get a narrower drawing, so labels stay readable when it is scaled.
  const narrow = typeof window !== "undefined" && window.innerWidth < 600;
  const W = narrow ? 420 : 720;
  const H = Math.round((opts.height ?? 320) * (narrow ? 0.85 : 1));
  const L = 48;
  const R = 12;
  const T = 12;
  const B = 40;
  const all = series.flatMap((s) => s.points).filter(([, y]) => Number.isFinite(y));
  const xMax = opts.xMax ?? Math.max(1, ...all.map(([x]) => x));
  // Gridlines at round steps (1, 2, 2.5 or 5 times a power of ten), from zero, or
  // from below zero when a line goes negative (debt).
  const hi = Math.max(0, ...all.map(([, y]) => y));
  const lo = Math.min(0, ...all.map(([, y]) => y));
  const yStep = (() => {
    const span = hi - lo;
    if (!(span > 0)) return 0.25;
    const target = span / 4;
    const p = 10 ** Math.floor(Math.log10(target));
    return [1, 2, 2.5, 5, 10].map((m) => m * p).find((c) => c >= target) ?? 10 * p;
  })();
  const yMin = Math.floor(lo / yStep) * yStep;
  const steps = Math.max(1, Math.ceil((hi - yMin) / yStep));
  const yMax = yMin + steps * yStep;
  const px = (x: number) => L + (x / xMax) * (W - L - R);
  const py = (y: number) => T + (H - T - B) - ((y - yMin) / (yMax - yMin)) * (H - T - B);

  let out = "";
  for (let i = 0; i <= steps; i++) {
    const v = yMin + yStep * i;
    out += `<line x1="${L}" y1="${py(v)}" x2="${W - R}" y2="${py(v)}" class="grid"/><text x="${L - 6}" y="${py(v) + 4}" text-anchor="end" class="tick">${fmtTick(v)}</text>`;
  }
  const xStep = xMax > 300 ? 60 : xMax > 120 ? 30 : xMax > 30 ? 10 : xMax > 12 ? 5 : 2;
  for (let x = 0; x <= xMax + 1e-9; x += xStep) {
    out += `<text x="${px(x)}" y="${H - B + 16}" text-anchor="middle" class="tick">${x}</text>`;
  }
  out += `<line x1="${L}" y1="${py(0)}" x2="${W - R}" y2="${py(0)}" class="axis-line"/>`;
  out += `<text x="${W - R}" y="${H - 6}" text-anchor="end" class="axis">${opts.xLabel}</text>`;
  out += `<text x="${L}" y="${T - 2}" class="axis" dy="-2">${opts.yLabel}</text>`;
  for (const s of series) {
    const pts = s.points.filter(([, y]) => Number.isFinite(y));
    if (pts.length === 0) continue;
    let d = "";
    pts.forEach(([x, y], i) => {
      if (i === 0) d += `M${px(x).toFixed(1)},${py(y).toFixed(1)}`;
      else if (s.step) d += `H${px(x).toFixed(1)}V${py(y).toFixed(1)}`;
      else d += `L${px(x).toFixed(1)},${py(y).toFixed(1)}`;
    });
    if (s.step) d += `H${px(xMax).toFixed(1)}`;
    out += `<path d="${d}" fill="none" stroke="${s.colour}" stroke-width="${s.width ?? 2}"${s.dash ? ` stroke-dasharray="${s.dash}"` : ""} stroke-linejoin="round"/>`;
  }
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
  svg.setAttribute("class", "chart");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", opts.title);
  svg.style.overflow = "visible";
  svg.innerHTML = out;

  const legend = h("ul", { class: "legend" });
  for (const s of series) {
    const li = document.createElement("li");
    li.innerHTML = `<svg viewBox="0 0 28 10" width="28" height="10" aria-hidden="true"><line x1="0" x2="28" y1="5" y2="5" stroke="${s.colour}" stroke-width="${s.width ?? 2}"${s.dash ? ` stroke-dasharray="${s.dash}"` : ""}/></svg>`;
    li.append(s.name);
    legend.append(li);
  }
  return h("div", { class: "line-chart" }, svg, legend);
}
