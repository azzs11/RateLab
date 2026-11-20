// Fig. 1: the boundary attack, live. Fixed window, sliding window and GCRA receive the
// same requests: `limit` just before a window edge and `limit` just after it.

import type { Core } from "../lib/wasm";
import { h, html } from "../lib/dom";
import { mark, surface } from "../lib/canvas";
import { icon } from "../lib/icons";
import { player } from "../lib/motion";
import { palette } from "../lib/palette";
import { onThemeChange } from "../lib/theme";
import { pretty } from "../lib/format";

const LIMIT = 10;
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

export function attackFigure(core: Core): { el: HTMLElement; dispose: () => void } {
  const traffic = core.Traffic.generate("boundary_attack", 0, SECS, LIMIT, WINDOW_MS, 1, 1);
  const at = Float64Array.from(traffic.atMs());
  const lanes: Lane[] = LANES.map((name) => {
    const run = core.LabRun.run(name, LIMIT, WINDOW_MS, traffic, 0, 1000);
    const allowed = Uint8Array.from(run.allowed());
    const lane = { name, at, allowed, stack: stacks(at, allowed), worst: run.worstWindow() };
    run.free();
    return lane;
  });
  traffic.free();

  const counters = lanes.map(() => h("span", { class: "num" }, "0"));
  const legend = h(
    "div",
    { class: "attack-legend" },
    ...lanes.map((lane, i) =>
      h(
        "div",
        { class: "attack-row" },
        h("span", { class: "attack-name" }, pretty(lane.name)),
        h(
          "span",
          { class: "attack-count" },
          "most in any 1 s: ",
          counters[i],
          h("span", { class: "muted" }, ` / limit ${LIMIT}`),
        ),
      ),
    ),
  );
  const live = h("p", { class: "visually-hidden", "aria-live": "polite" });

  let t = 0;
  const canvas = surface(
    `Timeline of the boundary attack. Fixed window admits up to ${lanes[0].worst} requests in one second, sliding window ${lanes[1].worst}, GCRA ${lanes[2].worst}; the limit is ${LIMIT}.`,
    2.1,
    (s) => {
      const p = palette();
      const { ctx, width, height } = s;
      const padL = 8;
      const padR = 8;
      const laneH = (height - 24) / lanes.length;
      const x = (ms: number) => padL + (ms / (SECS * 1000)) * (width - padL - padR);
      // Window edges.
      ctx.font = `11px ${p.mono}`;
      ctx.fillStyle = p.muted;
      for (let w = 0; w <= SECS; w++) {
        const xx = x(w * WINDOW_MS);
        ctx.strokeStyle = w % 2 === 1 ? p.lineStrong : p.line;
        ctx.setLineDash(w % 2 === 1 ? [] : [3, 4]);
        ctx.beginPath();
        ctx.moveTo(xx, 0);
        ctx.lineTo(xx, height - 20);
        ctx.stroke();
        ctx.setLineDash([]);
        if (w < SECS) ctx.fillText(`${w}s`, xx + 3, height - 6);
      }
      lanes.forEach((lane, li) => {
        const top = li * laneH;
        const base = top + laneH - 8;
        ctx.strokeStyle = p.line;
        ctx.beginPath();
        ctx.moveTo(padL, base + 4);
        ctx.lineTo(width - padR, base + 4);
        ctx.stroke();
        const r = Math.max(2.2, Math.min(3.6, (laneH - 24) / (LIMIT * 2.3)));
        const gap = r * 2.25;
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
      });
      // Lane labels on a pill, drawn last so the marks never cover them.
      ctx.font = `600 11px ${p.text}`;
      lanes.forEach((lane, li) => {
        const label = pretty(lane.name);
        const tw = ctx.measureText(label).width;
        ctx.fillStyle = p.surface;
        ctx.globalAlpha = 0.92;
        ctx.fillRect(padL + 1, li * laneH + 3, tw + 8, 16);
        ctx.globalAlpha = 1;
        ctx.fillStyle = p.ink2;
        ctx.fillText(label, padL + 5, li * laneH + 15);
      });
      // Playhead.
      ctx.strokeStyle = p.accent;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.moveTo(x(t), 0);
      ctx.lineTo(x(t), height - 20);
      ctx.stroke();
      ctx.lineWidth = 1;
    },
    220,
  );

  let lastAnnounced = "";
  const update = (time: number) => {
    t = time;
    canvas.redraw();
    lanes.forEach((lane, i) => {
      const n = worstSoFar(lane, t);
      counters[i].textContent = String(n);
      counters[i].parentElement?.classList.toggle("over", n > LIMIT);
    });
    const text = lanes.map((l, i) => `${pretty(l.name)} ${counters[i].textContent}`).join(", ");
    if (!playback.playing && text !== lastAnnounced) {
      live.textContent = `Most admitted in any one second so far: ${text}.`;
      lastAnnounced = text;
    }
  };

  const playBtn = h("button", { class: "btn ghost", type: "button" });
  // `wanted` is what the visitor chose; visibility only pauses temporarily.
  let wanted = true;
  let visible = true;
  const playback = player(update, {
    duration: SECS * 1000,
    loop: true,
    onState: (playing) => {
      playBtn.replaceChildren(html(playing ? icon.pause() : icon.play()), playing ? "Pause" : "Play");
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
  const replay = h(
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
    html(icon.restart()),
    "Attack again",
  );

  const stopTheme = onThemeChange(() => canvas.redraw());
  const el = h(
    "div",
    { class: "attack" },
    canvas.el,
    legend,
    h("div", { class: "row attack-actions" }, playBtn, replay, live),
  );
  playback.seek(0);
  sync();
  // Pause while scrolled out of view; resume when back, if it was playing.
  const io = new IntersectionObserver(([entry]) => {
    visible = entry.isIntersecting;
    sync();
  });
  requestAnimationFrame(() => io.observe(el));
  return {
    el,
    dispose: () => {
      playback.dispose();
      canvas.dispose();
      stopTheme();
      io.disconnect();
    },
  };
}
