// Goodput over time: successful answers per second for each concurrency limit, with
// the offered load and the most that could have succeeded. Lines differ by dash as
// well as colour. The numbers are in the table next to it; the chart is a picture.

export interface GoodputLine {
  limiter: string;
  points: { t: number; offered_rps: number; ok_rps: number; possible_rps: number }[];
}

/** Colour and dash per limiter (Okabe–Ito colours, as in RESULTS.md). */
export const LIMITER_STYLE: Record<string, { colour: string; dash: string; width: number }> = {
  none: { colour: "#E69F00", dash: "9 3 2 3", width: 2 },
  static: { colour: "#0072B2", dash: "", width: 2 },
  aimd: { colour: "#CC79A7", dash: "5 3", width: 2 },
  gradient2: { colour: "#009E73", dash: "", width: 3 },
};

const REFERENCE = {
  offered: { colour: "var(--muted)", dash: "2 3", width: 1.5 },
  possible: { colour: "var(--ink)", dash: "1 2", width: 2 },
};

function nice(v: number): number {
  if (v <= 0) return 1;
  const p = 10 ** Math.floor(Math.log10(v));
  return [1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10].map((m) => m * p).find((c) => c >= v) ?? 10 * p;
}

export function goodputChart(lines: GoodputLine[], title: string): SVGSVGElement {
  const W = 720;
  const H = 360;
  const L = 60;
  const R = 16;
  const T = 16;
  const B = 48;
  const all = lines.flatMap((l) => l.points);
  const step = all.length > 1 ? Math.abs(all[1].t - all[0].t) || 0.5 : 0.5;
  const xMax = Math.max(1, ...all.map((p) => p.t + step));
  const yMax = nice(Math.max(...all.map((p) => Math.max(p.offered_rps, p.ok_rps, p.possible_rps))));
  // Each bucket is drawn at its middle.
  const px = (t: number) => L + ((t + step / 2) / xMax) * (W - L - R);
  const py = (v: number) => T + (H - T - B) - (v / yMax) * (H - T - B);

  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
  svg.setAttribute("class", "pareto");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", title);

  let out = "";
  for (let i = 0; i <= 4; i++) {
    const v = (yMax * i) / 4;
    const y = py(v);
    out += `<line x1="${L}" y1="${y}" x2="${W - R}" y2="${y}" class="grid"/><text x="${L - 8}" y="${y + 4}" text-anchor="end" class="tick">${Math.round(v)}</text>`;
  }
  const xStep = xMax > 20 ? 5 : 2;
  for (let s = 0; s <= xMax + 1e-9; s += xStep) {
    const x = L + (s / xMax) * (W - L - R);
    out += `<text x="${x}" y="${H - B + 18}" text-anchor="middle" class="tick">${s}</text>`;
  }
  out += `<rect x="${L}" y="${T}" width="${W - L - R}" height="${H - T - B}" class="frame"/>`;
  out += `<text x="${L + (W - L - R) / 2}" y="${H - 10}" text-anchor="middle" class="axis">Seconds</text>`;
  out += `<text transform="translate(14 ${T + (H - T - B) / 2}) rotate(-90)" text-anchor="middle" class="axis">Requests per second</text>`;

  const poly = (pts: [number, number][], s: { colour: string; dash: string; width: number }) =>
    `<polyline points="${pts.map(([t, v]) => `${px(t).toFixed(1)},${py(v).toFixed(1)}`).join(" ")}" fill="none" stroke="${s.colour}" stroke-width="${s.width}"${s.dash ? ` stroke-dasharray="${s.dash}"` : ""} stroke-linejoin="round"/>`;
  const first = lines[0];
  if (first) {
    out += poly(first.points.map((p) => [p.t, p.offered_rps]), REFERENCE.offered);
    out += poly(first.points.map((p) => [p.t, p.possible_rps]), REFERENCE.possible);
  }
  for (const l of lines) {
    const s = LIMITER_STYLE[l.limiter] ?? { colour: "#777", dash: "", width: 2 };
    out += poly(l.points.map((p) => [p.t, p.ok_rps]), s);
  }
  svg.innerHTML = out;
  return svg;
}

/** Legend: the reference lines, then one entry per limiter. */
export function goodputLegend(): HTMLElement {
  const ul = document.createElement("ul");
  ul.className = "legend";
  const entries: [string, { colour: string; dash: string; width: number }][] = [
    ["offered", REFERENCE.offered],
    ["best possible", REFERENCE.possible],
    ...Object.entries(LIMITER_STYLE).map(([k, v]) => [`${k}`, v] as [string, typeof v]),
  ];
  for (const [name, s] of entries) {
    const li = document.createElement("li");
    li.innerHTML = `<svg viewBox="0 0 28 10" width="28" height="10" aria-hidden="true"><line x1="0" x2="28" y1="5" y2="5" stroke="${s.colour}" stroke-width="${s.width}"${s.dash ? ` stroke-dasharray="${s.dash}"` : ""}/></svg>${name}`;
    ul.appendChild(li);
  }
  return ul;
}
