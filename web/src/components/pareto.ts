// Interactive Pareto chart: store calls per 1,000 decisions against admission error.
// Each point is a button: keyboard and screen reader users can reach every one.

import { STRATEGY_STYLE } from "../lib/palette";
import { fmtPct, pretty } from "../lib/format";

export interface ParetoPoint {
  strategy: string;
  scenario: string;
  calls: number;
  error: number;
}

const symlog = (v: number) => Math.log10(1 + Math.max(0, v));

function shape(kind: string, x: number, y: number, r: number, colour: string): string {
  switch (kind) {
    case "square":
      return `<rect x="${x - r * 0.85}" y="${y - r * 0.85}" width="${r * 1.7}" height="${r * 1.7}" fill="${colour}"/>`;
    case "triangle":
      return `<polygon points="${x},${y - r} ${x - r},${y + r * 0.8} ${x + r},${y + r * 0.8}" fill="${colour}"/>`;
    case "diamond":
      return `<polygon points="${x},${y - r} ${x + r},${y} ${x},${y + r} ${x - r},${y}" fill="${colour}"/>`;
    case "cross":
      return `<path d="M${x - r},${y - r}L${x + r},${y + r}M${x - r},${y + r}L${x + r},${y - r}" stroke="${colour}" stroke-width="3"/>`;
    default:
      return `<circle cx="${x}" cy="${y}" r="${r}" fill="${colour}"/>`;
  }
}

/** Renders the chart. `onPick` is called when a point is activated. */
export function paretoChart(
  points: ParetoPoint[],
  opts: { title: string; onPick?: (p: ParetoPoint) => void; highlight?: string; selected?: string },
): SVGSVGElement {
  const W = 720;
  const H = 420;
  const L = 64;
  const R = 20;
  const T = 20;
  const B = 56;
  const maxCalls = Math.max(1000, ...points.map((p) => p.calls));
  const maxError = Math.max(5, ...points.map((p) => p.error));
  const xTicks = [0, 1, 10, 100, 1000, 10000].filter((t) => t <= maxCalls * 1.01);
  const yTicks = [0, 0.5, 1, 2, 5, 10, 20, 50, 100, 200].filter((t) => t <= maxError * 1.6);
  const xMax = Math.max(symlog(xTicks[xTicks.length - 1]), symlog(maxCalls));
  const yMax = Math.max(symlog(yTicks[yTicks.length - 1]), symlog(maxError));
  const px = (v: number) => L + (symlog(v) / xMax) * (W - L - R);
  const py = (v: number) => T + (H - T - B) - (symlog(v) / yMax) * (H - T - B);

  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
  svg.setAttribute("class", "pareto");
  svg.setAttribute("role", "group");
  svg.setAttribute("aria-label", opts.title);

  let grid = "";
  for (const t of xTicks) {
    const x = px(t);
    grid += `<line x1="${x}" y1="${T}" x2="${x}" y2="${H - B}" class="grid"/><text x="${x}" y="${H - B + 18}" text-anchor="middle" class="tick">${t}</text>`;
  }
  for (const t of yTicks) {
    const y = py(t);
    grid += `<line x1="${L}" y1="${y}" x2="${W - R}" y2="${y}" class="grid"/><text x="${L - 8}" y="${y + 4}" text-anchor="end" class="tick">${t}%</text>`;
  }
  grid += `<rect x="${L}" y="${T}" width="${W - L - R}" height="${H - T - B}" class="frame"/>`;
  grid += `<text x="${L + (W - L - R) / 2}" y="${H - 14}" text-anchor="middle" class="axis">Store calls per 1,000 decisions (log)</text>`;
  grid += `<text transform="translate(16 ${T + (H - T - B) / 2}) rotate(-90)" text-anchor="middle" class="axis">Admission error (log)</text>`;
  grid += `<text x="${L + 10}" y="${H - B - 10}" class="corner">better</text>`;
  svg.innerHTML = grid;

  for (const p of points) {
    const style = STRATEGY_STYLE[p.strategy] ?? { colour: "#777", shape: "circle" };
    const g = document.createElementNS(ns, "g");
    g.setAttribute("class", "point");
    g.setAttribute("tabindex", "0");
    g.setAttribute("role", "button");
    const label = `${pretty(p.strategy)}, ${p.scenario.replace(/_/g, " ")}: ${Math.round(p.calls)} calls per 1,000 decisions, ${fmtPct(p.error)} admission error`;
    g.setAttribute("aria-label", label);
    if (opts.highlight && opts.highlight !== p.scenario) g.setAttribute("data-dim", "true");
    if (opts.selected === `${p.strategy}:${p.scenario}`) {
      g.setAttribute("data-selected", "true");
      g.setAttribute("aria-pressed", "true");
    }
    g.innerHTML = `<title>${label}</title><circle cx="${px(p.calls)}" cy="${py(p.error)}" r="13" class="hit"/>${shape(style.shape, px(p.calls), py(p.error), 6.5, style.colour)}`;
    if (opts.onPick) {
      const pick = () => opts.onPick?.(p);
      g.addEventListener("click", pick);
      g.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          pick();
        }
      });
    }
    svg.appendChild(g);
  }
  return svg;
}

/** Legend with the same colours and shapes. */
export function paretoLegend(): HTMLElement {
  const ul = document.createElement("ul");
  ul.className = "legend";
  for (const [name, style] of Object.entries(STRATEGY_STYLE)) {
    const li = document.createElement("li");
    li.innerHTML = `<svg viewBox="0 0 20 20" width="16" height="16" aria-hidden="true">${shape(style.shape, 10, 10, 6, style.colour)}</svg>${pretty(name)}`;
    ul.appendChild(li);
  }
  return ul;
}
