import "@fontsource-variable/inter/wght.css";
import "@fontsource-variable/inter/wght-italic.css";
import "@fontsource-variable/jetbrains-mono/wght.css";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/views.css";

import inter from "@fontsource-variable/inter/files/inter-latin-wght-normal.woff2?url";
import { afterPaint, h, html, replace } from "./lib/dom";
import { applyStoredTheme, currentTheme, onThemeChange, toggleTheme } from "./lib/theme";
import { wasm } from "./lib/wasm";
import { loadResults, type Machine } from "./lib/data";
import * as home from "./views/home";

applyStoredTheme();
// Start the WebAssembly download once the first page has painted; the text font now.
void afterPaint().then(() => wasm());
document.head.append(
  Object.assign(document.createElement("link"), {
    rel: "preload",
    as: "font",
    type: "font/woff2",
    href: inter,
    crossOrigin: "anonymous",
  }),
);

export type Dispose = () => void;
export type View = (root: HTMLElement, params: URLSearchParams) => Dispose | Promise<Dispose>;

interface Route {
  path: string;
  label: string;
  title: string;
  group: "essay" | "instrument" | "appendix";
  load: () => Promise<{ default: View }>;
}

const ROUTES: Route[] = [
  { path: "", label: "Essay", title: "The price of exactness", group: "essay", load: () => Promise.resolve(home) },
  { path: "lab", label: "Lab", title: "Algorithm lab", group: "instrument", load: () => import("./views/lab") },
  { path: "cluster", label: "Cluster", title: "Cluster", group: "instrument", load: () => import("./views/cluster") },
  { path: "explorer", label: "Explorer", title: "Strategy explorer", group: "instrument", load: () => import("./views/explorer") },
  { path: "overload", label: "Overload", title: "Overload", group: "instrument", load: () => import("./views/overload") },
  { path: "replay", label: "Replay", title: "Replay your logs", group: "instrument", load: () => import("./views/replay") },
  { path: "results", label: "Results", title: "Results", group: "appendix", load: () => import("./views/results") },
];

const REPO = "https://github.com/azzs11/distributed-rate-limiter";

const app = document.getElementById("app")!;
const nav = h("nav", { class: "nav", "aria-label": "Sections" });
const themeBtn = h("button", { class: "theme-btn", type: "button", onclick: () => toggleTheme() });
const main = h("main", { id: "main", tabindex: "-1" });

function paintThemeButton() {
  const dark = currentTheme() === "dark";
  themeBtn.textContent = dark ? "Light" : "Dark";
  themeBtn.setAttribute("aria-label", dark ? "Switch to light theme" : "Switch to dark theme");
}
paintThemeButton();
onThemeChange(paintThemeButton);

for (const r of ROUTES) {
  if (r.path === "lab") nav.append(h("span", { class: "group", "aria-hidden": "true" }, "Instruments:"));
  nav.append(h("a", { href: `#/${r.path}`, "data-path": r.path }, r.label));
}

/** The mark: requests up to a window edge, the edge, and the ones after it. */
const MARK =
  '<svg viewBox="0 0 24 16" width="22" height="15" aria-hidden="true"><g stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M2 5v9M6 5v9M10 5v9"/><path d="M22 5v9M18 5v9" opacity=".4"/></g><path d="M14 1v15" stroke="var(--accent)" stroke-width="1.8" stroke-linecap="round"/></svg>';

const measured = h("span", {});
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
        h("span", { class: "mark" }, html(MARK), "Ratelab"),
        h("span", { class: "sub" }, "a rate limiter that measures itself"),
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
        "Written and built by ",
        h("a", { href: "https://github.com/azzs11" }, "Akchhya Singh"),
        ". Every number here comes from the project's own Rust code: computed in your browser as you read, or by the benchmark in ",
        h("a", { href: `${REPO}/blob/main/results/RESULTS.md` }, "results/RESULTS.md"),
        ".",
        measured,
      ),
      h("p", {}, h("a", { href: REPO }, "Source"), " · MIT licence"),
    ),
  ),
);
void afterPaint()
  .then(() => loadResults<{ machine: Machine }>("local"))
  .then((l) => {
  if (l) measured.textContent = ` Benchmark last run ${l.machine.measured_at.slice(0, 10)} at commit ${l.machine.git_commit.slice(0, 7)}.`;
});

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
  document.title = r.path ? `${r.title} · Ratelab` : "Ratelab: the price of exactness in rate limiting";

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
