// Light/dark theme: follows the system until the visitor picks one.

const KEY = "ratelab-theme";

function stored(): "light" | "dark" | null {
  try {
    const v = localStorage.getItem(KEY);
    return v === "light" || v === "dark" ? v : null;
  } catch {
    return null;
  }
}

export function currentTheme(): "light" | "dark" {
  const s = stored();
  if (s) return s;
  return matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function applyStoredTheme(): void {
  const s = stored();
  if (s) document.documentElement.dataset.theme = s;
}

const listeners = new Set<() => void>();

/** Called when the theme changes, so canvases can repaint. */
export function onThemeChange(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

export function toggleTheme(): void {
  const next = currentTheme() === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = next;
  try {
    localStorage.setItem(KEY, next);
  } catch {
    // Storage may be unavailable (private mode); the choice lasts for this page only.
  }
  listeners.forEach((fn) => fn());
}

matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
  if (!stored()) listeners.forEach((fn) => fn());
});
