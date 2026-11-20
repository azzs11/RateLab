// Controls that live inside sentences, in the spirit of Bret Victor's "reactive
// documents": a number you drag sideways (or step with the arrow keys), a choice that
// opens a native menu, a phrase that switches on and off. They read as part of the
// prose, so the question being asked and the knob that changes it are the same words.

import { h } from "./dom";

export interface ScrubOptions {
  min: number;
  max: number;
  step: number;
  value: number;
  /** How the value reads in the sentence. */
  format: (v: number) => string;
  /** What the value is, for screen readers ("limit", "load"). */
  label: string;
  /** Pixels of drag per step (default 6). */
  pixels?: number;
}

export interface Scrub {
  el: HTMLElement;
  value: () => number;
  set: (v: number) => void;
}

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** A draggable number. Arrow keys step, Page keys take ten steps, Home and End jump. */
export function scrub(o: ScrubOptions, onChange: (v: number) => void): Scrub {
  let value = o.value;
  const el = h("span", {
    class: "scrub",
    role: "slider",
    tabindex: "0",
    "aria-label": o.label,
    "aria-valuemin": String(o.min),
    "aria-valuemax": String(o.max),
    title: "Drag sideways, or use the arrow keys",
  });
  const snap = (v: number) => {
    const n = Math.round((v - o.min) / o.step) * o.step + o.min;
    return clamp(Number(n.toFixed(6)), o.min, o.max);
  };
  const paint = () => {
    el.textContent = o.format(value);
    el.setAttribute("aria-valuenow", String(value));
    el.setAttribute("aria-valuetext", o.format(value));
  };
  const set = (v: number, notify = true) => {
    const next = snap(v);
    if (next === value) return;
    value = next;
    paint();
    if (notify) onChange(value);
  };
  paint();

  let startX = 0;
  let startValue = 0;
  el.addEventListener("pointerdown", (e) => {
    startX = e.clientX;
    startValue = value;
    el.setPointerCapture(e.pointerId);
    el.dataset.dragging = "true";
    e.preventDefault();
  });
  el.addEventListener("pointermove", (e) => {
    if (el.dataset.dragging !== "true") return;
    const steps = Math.round((e.clientX - startX) / (o.pixels ?? 6));
    set(startValue + steps * o.step);
  });
  const end = () => {
    delete el.dataset.dragging;
  };
  el.addEventListener("pointerup", end);
  el.addEventListener("pointercancel", end);
  el.addEventListener("keydown", (e) => {
    const big = o.step * 10;
    const moves: Record<string, number> = {
      ArrowRight: value + o.step,
      ArrowUp: value + o.step,
      ArrowLeft: value - o.step,
      ArrowDown: value - o.step,
      PageUp: value + big,
      PageDown: value - big,
      Home: o.min,
      End: o.max,
    };
    if (e.key in moves) {
      e.preventDefault();
      set(moves[e.key]);
    }
  });
  return { el, value: () => value, set: (v) => set(v, false) };
}

/** A choice inside a sentence: a native select, styled as a word. */
export function choose(
  label: string,
  options: { value: string; text: string }[],
  value: string,
  onChange: (v: string) => void,
): { el: HTMLElement; select: HTMLSelectElement } {
  const select = h("select", { "aria-label": label }) as HTMLSelectElement;
  for (const o of options) {
    const opt = h("option", { value: o.value }, o.text);
    if (o.value === value) opt.selected = true;
    select.append(opt);
  }
  select.addEventListener("change", () => onChange(select.value));
  return { el: h("span", { class: "choose" }, select), select };
}

/** A phrase that switches on and off ("with a store outage"). */
export function toggle(text: string, on: boolean, onChange: (on: boolean) => void): HTMLButtonElement {
  const b = h("button", { class: "toggle", type: "button", "aria-pressed": String(on) }, text) as HTMLButtonElement;
  b.addEventListener("click", () => {
    const next = b.getAttribute("aria-pressed") !== "true";
    b.setAttribute("aria-pressed", String(next));
    onChange(next);
  });
  return b;
}

/** A paragraph mixing words and controls. */
export function sentence(...parts: (string | Node)[]): HTMLElement {
  return h("p", { class: "sentence" }, ...parts);
}

/** The one-line hint under the first sentence of a page. */
export function howTo(): HTMLElement {
  return h(
    "p",
    { class: "how-to" },
    "Underlined values are controls: drag a number sideways or focus it and use the arrow keys; pick a word to change it.",
  );
}
