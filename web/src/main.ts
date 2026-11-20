import "@fontsource/instrument-serif/400-italic.css";
import "@fontsource/instrument-serif/400.css";
import "@fontsource-variable/inter/wght.css";
import "@fontsource-variable/jetbrains-mono/wght.css";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/views.css";

import serifItalic from "@fontsource/instrument-serif/files/instrument-serif-latin-400-italic.woff2?url";
import { h, html, replace } from "./lib/dom";
import { icon } from "./lib/icons";
import { applyStoredTheme, currentTheme, onThemeChange, toggleTheme } from "./lib/theme";
import { wasm } from "./lib/wasm";
import * as home from "./views/home";

applyStoredTheme();
// Start the WebAssembly download and the display font now, in parallel with rendering.
void wasm();
document.head.append(
  Object.assign(document.createElement("link"), {
    rel: "preload",
    as: "font",
    type: "font/woff2",
    href: serifItalic,
    crossOrigin: "anonymous",
  }),
);

export type Dispose = () => void;
export type View = (root: HTMLElement, params: URLSearchParams) => Dispose | Promise<Dispose>;

interface Route {
  path: string;
  label: string;
  title: string;
  load: () => Promise<{ default: View }>;
}

const ROUTES: Route[] = [
  { path: "", label: "Story", title: "Rate limiting, measured", load: () => Promise.resolve(home) },
  { path: "lab", label: "Lab", title: "Algorithm lab", load: () => import("./views/lab") },
  { path: "cluster", label: "Cluster", title: "Cluster simulator", load: () => import("./views/cluster") },
  { path: "explorer", label: "Explorer", title: "Strategy explorer", load: () => import("./views/explorer") },
  { path: "replay", label: "Replay", title: "Replay your logs", load: () => import("./views/replay") },
  { path: "results", label: "Results", title: "Results", load: () => import("./views/results") },
];

const REPO = "https://github.com/azzs11/distributed-rate-limiter";

const app = document.getElementById("app")!;
const nav = h("nav", { class: "nav", "aria-label": "Sections" });
const themeBtn = h("button", { class: "icon-btn", type: "button", onclick: () => toggleTheme() });
const main = h("main", { id: "main", tabindex: "-1" });

function paintThemeButton() {
  const dark = currentTheme() === "dark";
  themeBtn.replaceChildren(html(dark ? icon.sun() : icon.moon()));
  themeBtn.setAttribute("aria-label", dark ? "Switch to light theme" : "Switch to dark theme");
}
paintThemeButton();
onThemeChange(paintThemeButton);

for (const r of ROUTES) {
  nav.append(h("a", { href: `#/${r.path}`, "data-path": r.path }, r.label));
}

app.append(
  h(
    "header",
    { class: "site-header" },
    h(
      "div",
      { class: "page" },
      h(
        "a",
        { class: "brand", href: "#/" },
        h("span", { class: "mark" }, "Ratelab"),
        h("span", { class: "sub" }, "rate limiting, measured"),
      ),
      nav,
      themeBtn,
    ),
  ),
  main,
  h(
    "footer",
    { class: "site-footer" },
    h(
      "div",
      { class: "page" },
      h(
        "p",
        {},
        "Every number on this site is computed by the project's own Rust code: live in your browser, or by the benchmark in ",
        h("a", { href: `${REPO}/blob/main/results/RESULTS.md` }, "results/RESULTS.md"),
        ".",
      ),
      h("p", {}, h("a", { href: REPO }, "Source on GitHub"), " · MIT licence"),
    ),
  ),
);

/** Horizontally scrolling tables and code must be reachable by keyboard. */
function focusableScrollers(scope: ParentNode) {
  scope.querySelectorAll<HTMLElement>(".table-wrap, pre").forEach((el) => {
    if (el.hasAttribute("tabindex")) return;
    el.tabIndex = 0;
    el.setAttribute("role", "region");
    const caption = el.querySelector("caption")?.textContent?.trim();
    el.setAttribute("aria-label", caption || (el.tagName === "PRE" ? "Code example" : "Table"));
  });
}
new MutationObserver(() => focusableScrollers(main)).observe(main, { childList: true, subtree: true });

let dispose: Dispose | null = null;
let generation = 0;

async function route() {
  const hash = location.hash.replace(/^#\/?/, "");
  const [path, query = ""] = hash.split("?");
  const r = ROUTES.find((x) => x.path === path) ?? ROUTES[0];
  const params = new URLSearchParams(query);
  const mine = ++generation;

  for (const a of nav.querySelectorAll("a")) {
    if (a.dataset.path === r.path) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  }
  document.title = r.path ? `${r.title} · Ratelab` : "Ratelab · rate limiting, measured";

  dispose?.();
  dispose = null;
  if (r.path) replace(main, h("div", { class: "page loading", role: "status" }, "Loading…"));
  try {
    const mod = await r.load();
    if (mine !== generation) return;
    const root = h("div", { class: "view" });
    replace(main, root);
    const d = await mod.default(root, params);
    if (mine !== generation) {
      d();
      return;
    }
    dispose = d;
    window.scrollTo({ top: 0 });
    // Move focus to the new page's heading for screen reader and keyboard users.
    const heading = root.querySelector<HTMLElement>("h1");
    if (heading && location.hash) {
      heading.tabIndex = -1;
      heading.focus({ preventScroll: true });
    }
  } catch (e) {
    replace(
      main,
      h(
        "div",
        { class: "page section" },
        h("p", { class: "error-box", role: "alert" }, `This view failed to load: ${String(e)}`),
      ),
    );
  }
}

window.addEventListener("hashchange", route);
route();
