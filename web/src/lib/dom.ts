// Tiny DOM helpers: no framework, so the page stays small and fast.

type Attrs = Record<string, string | number | boolean | undefined | null | EventListener>;
type Child = Node | string | number | null | undefined | false;

/** Creates an element. `on*` attributes become event listeners. */
export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Attrs = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === null || value === false) continue;
    if (key.startsWith("on") && typeof value === "function") {
      el.addEventListener(key.slice(2).toLowerCase(), value as EventListener);
    } else if (value === true) {
      el.setAttribute(key, "");
    } else {
      el.setAttribute(key, String(value));
    }
  }
  append(el, ...children);
  return el;
}

/** Appends children, skipping empty ones. */
export function append(parent: Node, ...children: Child[]): void {
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    parent.appendChild(typeof c === "object" ? c : document.createTextNode(String(c)));
  }
}

/** Parses trusted markup written in this codebase (icons, small fragments). */
export function html(markup: string): DocumentFragment {
  const t = document.createElement("template");
  t.innerHTML = markup.trim();
  return t.content;
}

/** Replaces all children. */
export function replace(parent: Element, ...children: Child[]): void {
  parent.replaceChildren();
  append(parent, ...children);
}

let ids = 0;
/** A unique id for label/aria wiring. */
export function uid(prefix = "id"): string {
  ids += 1;
  return `${prefix}-${ids}`;
}

/** Resolves after the browser has painted the current DOM, so downloads and work that
 * start after it never hold up the first paint. */
export function afterPaint(): Promise<void> {
  return new Promise((resolve) => requestAnimationFrame(() => setTimeout(resolve, 0)));
}
