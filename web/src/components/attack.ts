// Figure 1: the boundary attack, live. Fixed window, sliding window and GCRA receive the
// same requests: `limit` just before a window edge and `limit` just after it. When a
// lane lets more than the limit through in one second, the figure marks where.

import type { Core } from "../lib/wasm";
import { h } from "../lib/dom";
import { mark, surface } from "../lib/canvas";
import { player } from "../lib/motion";
import { palette } from "../lib/palette";
import { onThemeChange } from "../lib/theme";
import { pretty } from "../lib/format";

const WINDOW_MS = 1000;
const SECS = 6;
const LANES = ["fixed_window", "sliding_window", "gcra"] as const;

interface Lane {
  name: string;
  at: Float64Array;
  allowed: Uint8Array;
  /** Position within its column: same wave, same outcome. */
  stack: Uint16Array;
  worst: number;
}

/** Stacks requests of the same instant: allowed and blocked in separate columns. */
function stacks(at: Float64Array, allowed: Uint8Array): Uint16Array {
  const out = new Uint16Array(at.length);
  const seen = new Map<string, number>();
  for (let i = 0; i < at.length; i++) {
    const key = `${at[i]}:${allowed[i]}`;
    const n = seen.get(key) ?? 0;
    out[i] = n;
    seen.set(key, n + 1);
  }
  return out;
}

/** Most admitted requests in any window-long span ending at or before `t`. */
function worstSoFar(lane: Lane, t: number): number {
  let best = 0;
  let lo = 0;
  let count = 0;
  const admitted: number[] = [];
  for (let i = 0; i < lane.at.length && lane.at[i] <= t; i++) {
    if (!lane.allowed[i]) continue;
    admitted.push(lane.at[i]);
    count++;
    while (admitted[admitted.length - 1] - admitted[lo] >= WINDOW_MS) {
      lo++;
      count--;
    }
    best = Math.max(best, count);
  }
  return best;
}

function build(core: Core, limit: number): Lane[] {
  const traffic = core.Traffic.generate("boundary_attack", 0, SECS, limit, WINDOW_MS, 1, 1);
  const at = Float64Array.from(traffic.atMs());
  const lanes = LANES.map((name) => {
    const run = core.LabRun.run(name, limit, WINDOW_MS, traffic, 0, 1000);
    const allowed = Uint8Array.from(run.allowed());
    const lane = { name, at, allowed, stack: stacks(at, allowed), worst: run.worstWindow() };
    run.free();
    return lane;
  });
  traffic.free();
  return lanes;
}

export interface AttackFigure {
  el: HTMLElement;
  /** Rebuilds for a new limit and replays from the start. */
  setLimit: (limit: number) => void;
  /** Worst one-second count per lane for the current limit, keyed by algorithm. */
  worst: () => Record<string, number>;
  dispose: () => void;
}

export function attackFigure(core: Core, initialLimit = 10): AttackFigure {
  let limit = initialLimit;
  let lanes = build(core, limit);

  const counters = LANES.map(() => h("td", { class: "n" }, "0"));
  const legend = h(
    "table",
    { class: "attack-legend" },
    h("caption", { class: "visually-hidden" }, "Most requests admitted in any one second, so far"),
    h("thead", {}, h("tr", {}, h("th", { scope: "col" }, "Algorithm"), h("th", { scope: "col", class: "n" }, "Most in any 1 s, so far"))),
    h("tbody", {}, ...LANES.map((name, i) => h("tr", {}, h("th", { scope: "row" }, pretty(name)), counters[i]))),
  );
  const live = h("p", { class: "visually-hidden", "aria-live": "polite" });

  let t = 0;
  const canvas = surface(
    "Timeline of the boundary attack",
    2.3,
    (s) => {
      const p = palette();
      const { ctx, width, height } = s;
      const narrow = width < 560;
      const padL = narrow ? 8 : 112;
      const padR = 12;
      const laneH = (height - 26) / lanes.length;
      const x = (ms: number) => padL + (ms / (SECS * 1000)) * (width - padL - padR);
      ctx.font = `11px ${p.mono}`;
      ctx.fillStyle = p.muted;
      ctx.textBaseline = "alphabetic";
      for (let w = 0; w <= SECS; w++) {
        const xx = x(w * WINDOW_MS);
        ctx.strokeStyle = p.line;
        ctx.beginPath();
        ctx.moveTo(xx, 0);
        ctx.lineTo(xx, height - 22);
        ctx.stroke();
        ctx.textAlign = w === SECS ? "right" : "left";
        ctx.fillText(`${w} s`, w === SECS ? xx - 2 : xx + 3, height - 6);
        ctx.textAlign = "left";
      }
      const r = Math.max(2, Math.min(3.4, (laneH - 30) / (limit * 2.3)));
      const gap = r * 2.25;
      lanes.forEach((lane, li) => {
        const top = li * laneH;
        const base = top + laneH - 8;
        ctx.strokeStyle = p.line;
        ctx.beginPath();
        ctx.moveTo(padL, base + 4);
        ctx.lineTo(width - padR, base + 4);
        ctx.stroke();
        ctx.font = `500 12px ${p.text}`;
        ctx.fillStyle = p.ink;
        if (narrow) ctx.fillText(pretty(lane.name), padL + 4, top + 14);
        else ctx.fillText(pretty(lane.name), 0, base);
        for (let i = 0; i < lane.at.length && lane.at[i] <= t; i++) {
          const ok = lane.allowed[i] === 1;
          // The two waves are 2 ms apart, too close to see: each is drawn on its side
          // of the window edge, allowed and blocked in neighbouring columns.
          const phase = lane.at[i] % WINDOW_MS;
          const side = phase > WINDOW_MS / 2 ? -1 : 1;
          const column = ok ? 1 : 2;
          const xx = x(Math.round(lane.at[i] / WINDOW_MS) * WINDOW_MS) + side * column * (gap + 2);
          const yy = base - lane.stack[i] * gap - r;
          mark(ctx, xx, yy, r, ok, p.allowed, p.blocked);
        }
        // Annotation: where this lane broke the limit.
        const n = worstSoFar(lane, t);
        if (n > limit) {
          const edge = x(WINDOW_MS);
          const left = edge - (gap + 2) - r - 3;
          const right = edge + (gap + 2) + r + 3;
          const y = base - limit * gap - r - 6;
          ctx.strokeStyle = p.accent;
          ctx.lineWidth = 1.2;
          ctx.beginPath();
          ctx.moveTo(left, y + 5);
          ctx.lineTo(left, y);
          ctx.lineTo(right, y);
          ctx.lineTo(right, y + 5);
          ctx.moveTo(right, y);
          ctx.lineTo(right + 18, y - 8);
          ctx.stroke();
          ctx.lineWidth = 1;
          ctx.fillStyle = p.accent;
          ctx.font = `italic 500 12px ${p.text}`;
          ctx.fillText(width < 560 ? `${n} in 1 s` : `${n} let through within one second (limit ${limit})`, right + 22, y - 5);
        }
      });
      ctx.strokeStyle = p.ink;
      ctx.globalAlpha = 0.5;
      ctx.beginPath();
      ctx.moveTo(x(t), 0);
      ctx.lineTo(x(t), height - 22);
      ctx.stroke();
      ctx.globalAlpha = 1;
    },
    230,
  );

  const describe = () =>
    canvas.surface.canvas.setAttribute(
      "aria-label",
      `Timeline of the boundary attack with a limit of ${limit} per second. Most admitted in any one second: ${lanes
        .map((l) => `${pretty(l.name)} ${l.worst}`)
        .join(", ")}.`,
    );
  describe();

  let lastAnnounced = "";
  const update = (time: number) => {
    t = time;
    canvas.redraw();
    lanes.forEach((lane, i) => {
      const n = worstSoFar(lane, t);
      counters[i].textContent = String(n);
      counters[i].classList.toggle("over", n > limit);
    });
    const text = lanes.map((l, i) => `${pretty(l.name)} ${counters[i].textContent}`).join(", ");
    if (!playback.playing && text !== lastAnnounced) {
      live.textContent = `Most admitted in any one second so far: ${text}.`;
      lastAnnounced = text;
    }
  };

  const playBtn = h("button", { class: "btn ghost", type: "button" });
  let wanted = true;
  let visible = true;
  const playback = player(update, {
    duration: SECS * 1000,
    loop: true,
    onState: (playing) => {
      playBtn.textContent = playing ? "Pause" : "Play";
      playBtn.setAttribute("aria-pressed", String(playing));
    },
  });
  const sync = () => {
    if (wanted && visible) playback.play();
    else playback.pause();
  };
  playBtn.addEventListener("click", () => {
    wanted = !playback.playing;
    sync();
  });
  const again = h(
    "button",
    {
      class: "btn ghost",
      type: "button",
      onclick: () => {
        wanted = true;
        playback.seek(0);
        sync();
      },
    },
    "Start again",
  );

  const stopTheme = onThemeChange(() => canvas.redraw());
  const el = h(
    "div",
    { class: "attack" },
    canvas.el,
    h("div", { class: "attack-foot" }, legend, h("div", { class: "row" }, playBtn, again)),
    live,
  );
  playback.seek(0);
  sync();
  const io = new IntersectionObserver(([entry]) => {
    visible = entry.isIntersecting;
    sync();
  });
  requestAnimationFrame(() => io.observe(el));
  return {
    el,
    setLimit: (next) => {
      limit = next;
      lanes = build(core, limit);
      describe();
      playback.seek(0);
      wanted = true;
      sync();
    },
    worst: () => Object.fromEntries(lanes.map((l) => [l.name, l.worst])),
    dispose: () => {
      playback.dispose();
      canvas.dispose();
      stopTheme();
      io.disconnect();
    },
  };
}
