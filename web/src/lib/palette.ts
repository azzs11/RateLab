// Reads the design tokens so canvases use exactly the CSS colours of the theme.

export interface Palette {
  bg: string;
  surface: string;
  surface2: string;
  ink: string;
  ink2: string;
  muted: string;
  line: string;
  lineStrong: string;
  accent: string;
  accentSoft: string;
  allowed: string;
  allowedSoft: string;
  blocked: string;
  blockedSoft: string;
  mono: string;
  text: string;
  display: string;
}

export function palette(): Palette {
  const s = getComputedStyle(document.documentElement);
  const v = (name: string) => s.getPropertyValue(name).trim();
  return {
    bg: v("--bg"),
    surface: v("--surface"),
    surface2: v("--surface-2"),
    ink: v("--ink"),
    ink2: v("--ink-2"),
    muted: v("--muted"),
    line: v("--line"),
    lineStrong: v("--line-strong"),
    accent: v("--accent"),
    accentSoft: v("--accent-soft"),
    allowed: v("--allowed"),
    allowedSoft: v("--allowed-soft"),
    blocked: v("--blocked"),
    blockedSoft: v("--blocked-soft"),
    mono: v("--font-mono"),
    text: v("--font-text"),
    display: v("--font-display"),
  };
}

/** Strategy colours (Okabe–Ito, colour-blind safe) with a marker shape each. */
export const STRATEGY_STYLE: Record<string, { colour: string; shape: "circle" | "square" | "triangle" | "diamond" | "cross" }> = {
  central_exact: { colour: "#0072B2", shape: "circle" },
  local_split: { colour: "#E69F00", shape: "square" },
  async_sync: { colour: "#CC79A7", shape: "triangle" },
  fixed_leasing: { colour: "#009E73", shape: "diamond" },
  adaptive_leasing: { colour: "#D55E00", shape: "cross" },
};
