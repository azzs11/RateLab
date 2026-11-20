// Number formatting: tabular, honest precision.

export const fmtInt = (n: number): string =>
  Number.isFinite(n) ? Math.round(n).toLocaleString("en-US") : "–";

export const fmtPct = (n: number, digits = 2): string =>
  Number.isFinite(n) ? `${n.toFixed(digits)}%` : "–";

export const fmtNum = (n: number, digits = 1): string =>
  Number.isFinite(n) ? n.toLocaleString("en-US", { maximumFractionDigits: digits, minimumFractionDigits: digits }) : "–";

export const fmtMs = (ms: number): string => {
  if (!Number.isFinite(ms)) return "–";
  if (ms >= 1000) return `${(ms / 1000).toFixed(2)} s`;
  if (ms >= 1) return `${ms.toFixed(ms >= 100 ? 0 : 1)} ms`;
  return `${(ms * 1000).toFixed(0)} µs`;
};

export const fmtRate = (perSec: number): string => {
  if (!Number.isFinite(perSec)) return "–";
  if (perSec >= 1e6) return `${(perSec / 1e6).toFixed(2)} M/s`;
  if (perSec >= 1e3) return `${(perSec / 1e3).toFixed(1)} k/s`;
  return `${perSec.toFixed(0)}/s`;
};

/** snake_case → "Snake case" for display. */
export const pretty = (name: string): string =>
  name === "gcra"
    ? "GCRA"
    : name.replace(/_/g, " ").replace(/^\w/, (c) => c.toUpperCase());
