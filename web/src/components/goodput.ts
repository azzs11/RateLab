// Goodput over time: successful answers per second for each concurrency limit, with
// the offered load and the most that could have succeeded.

import { pretty } from "../lib/format";
import { lineChart, type Series } from "./lines";

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

/** The chart plus its legend. Buckets are drawn at their middle. */
export function goodputChart(lines: GoodputLine[], title: string): HTMLElement {
  const first = lines[0]?.points ?? [];
  const step = first.length > 1 ? first[1].t - first[0].t : 0.5;
  const mid = (t: number) => t + step / 2;
  const series: Series[] = [
    { name: "offered", colour: "var(--muted)", dash: "2 3", width: 1.5, points: first.map((p) => [mid(p.t), p.offered_rps]) },
    { name: "most that could succeed", colour: "var(--ink)", dash: "1 2", width: 2, points: first.map((p) => [mid(p.t), p.possible_rps]) },
    ...lines.map((l) => {
      const s = LIMITER_STYLE[l.limiter] ?? { colour: "#777", dash: "", width: 2 };
      return { name: `${pretty(l.limiter)}: answered in time`, ...s, points: l.points.map((p) => [mid(p.t), p.ok_rps] as [number, number]) };
    }),
  ];
  const xMax = first.length ? first[first.length - 1].t + step : 1;
  return lineChart(series, { title, xLabel: "seconds", yLabel: "requests per second", xMax });
}
