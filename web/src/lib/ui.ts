// Labelled form controls with consistent markup and accessible names.

import { h, html, uid } from "./dom";
import { icon } from "./icons";
import type { Player } from "./motion";
import { fmtMs } from "./format";

export function selectField(
  label: string,
  options: { value: string; text: string }[],
  value: string,
  onChange: (v: string) => void,
): { el: HTMLElement; input: HTMLSelectElement } {
  const id = uid("sel");
  const input = h("select", { id }) as HTMLSelectElement;
  for (const o of options) {
    const opt = h("option", { value: o.value }, o.text);
    if (o.value === value) opt.selected = true;
    input.append(opt);
  }
  input.addEventListener("change", () => onChange(input.value));
  return { el: h("div", { class: "field" }, h("label", { for: id }, label), input), input };
}

export function rangeField(
  label: string,
  opts: { min: number; max: number; step: number; value: number; format: (v: number) => string },
  onChange: (v: number) => void,
): { el: HTMLElement; input: HTMLInputElement } {
  const id = uid("rng");
  const out = h("output", { for: id }, opts.format(opts.value));
  const input = h("input", {
    id,
    type: "range",
    min: opts.min,
    max: opts.max,
    step: opts.step,
    value: opts.value,
  }) as HTMLInputElement;
  input.addEventListener("input", () => {
    out.textContent = opts.format(Number(input.value));
    input.setAttribute("aria-valuetext", opts.format(Number(input.value)));
    onChange(Number(input.value));
  });
  input.setAttribute("aria-valuetext", opts.format(opts.value));
  return {
    el: h("div", { class: "field" }, h("label", { for: id }, label, out), input),
    input,
  };
}

export function checkField(label: string, value: boolean, onChange: (v: boolean) => void): HTMLElement {
  const input = h("input", { type: "checkbox" }) as HTMLInputElement;
  input.checked = value;
  input.addEventListener("change", () => onChange(input.checked));
  return h("label", { class: "check" }, input, label);
}

/** Play/pause, a time scrubber and a speed switch bound to a player. */
export function playerBar(p: Player, duration: () => number, speeds: number[], setSpeed: (s: number) => void) {
  const play = h("button", { class: "icon-btn", type: "button" }) as HTMLButtonElement;
  const paint = (playing: boolean) => {
    play.replaceChildren(html(playing ? icon.pause() : icon.play()));
    play.setAttribute("aria-label", playing ? "Pause" : "Play");
  };
  paint(false);
  play.addEventListener("click", () => p.toggle());
  const scrubId = uid("scrub");
  const scrub = h("input", {
    id: scrubId,
    type: "range",
    min: 0,
    max: 1000,
    step: 1,
    value: 0,
    "aria-label": "Time",
  }) as HTMLInputElement;
  scrub.addEventListener("input", () => {
    p.pause();
    p.seek((Number(scrub.value) / 1000) * duration());
  });
  const time = h("span", { class: "time", "aria-hidden": "true" }, "0 ms");
  const speed = h("div", { class: "segmented", role: "group", "aria-label": "Playback speed" });
  for (const s of speeds) {
    const b = h("button", { type: "button", "aria-pressed": String(s === 1) }, `${s}×`);
    b.addEventListener("click", () => {
      speed.querySelectorAll("button").forEach((x) => x.setAttribute("aria-pressed", "false"));
      b.setAttribute("aria-pressed", "true");
      setSpeed(s);
    });
    speed.append(b);
  }
  const update = (t: number) => {
    const d = duration();
    scrub.value = String(Math.round((t / Math.max(1, d)) * 1000));
    scrub.setAttribute("aria-valuetext", `${fmtMs(t)} of ${fmtMs(d)}`);
    time.textContent = `${fmtMs(t)} / ${fmtMs(d)}`;
  };
  return { el: h("div", { class: "player" }, play, scrub, time, speed), update, paint };
}
