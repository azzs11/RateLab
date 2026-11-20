import { h } from "../lib/dom";

export default function view(root: HTMLElement) {
  root.append(h("div", { class: "page view-head" }, h("h1", { class: "display" }, "cluster")));
  return () => {};
}
