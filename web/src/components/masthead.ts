import { h } from "../lib/dom";

/** The top of a page: where it sits, what it is, one sentence on why. */
export function masthead(eyebrow: string, title: string, standfirst: string | Node): HTMLElement {
  return h(
    "header",
    { class: "masthead" },
    h(
      "div",
      { class: "page" },
      h("div", { class: "prose" }, h("p", { class: "eyebrow" }, eyebrow), h("h1", {}, title), h("p", { class: "standfirst" }, standfirst)),
    ),
  );
}
