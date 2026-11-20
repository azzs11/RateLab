import { h } from "../lib/dom";

/** A numbered figure: body plus caption, like a figure in a paper. */
export function figure(
  number: string,
  title: string,
  caption: string | Node,
  ...body: (Node | null)[]
): HTMLElement {
  return h(
    "figure",
    { class: "figure", style: "margin:0" },
    ...body,
    h(
      "figcaption",
      { class: "figure-caption" },
      h("span", { class: "fig" }, `FIG. ${number}`),
      h("em", {}, title),
      typeof caption === "string" ? h("span", {}, caption) : caption,
    ),
  );
}
