// Inline SVG icons (stroke = currentColor), so meaning never depends on colour alone.

const svg = (body: string, label?: string): string =>
  `<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" ${
    label ? `role="img" aria-label="${label}"` : 'aria-hidden="true"'
  }>${body}</svg>`;

export const icon = {
  check: (label?: string) => svg('<path d="M5 12.5l4.5 4.5L19 7.5"/>', label),
  cross: (label?: string) => svg('<path d="M6 6l12 12M18 6L6 18"/>', label),
  play: () => svg('<path d="M7 5l12 7-12 7z" fill="currentColor" stroke="none"/>'),
  pause: () => svg('<path d="M7 5h3.5v14H7zM13.5 5H17v14h-3.5z" fill="currentColor" stroke="none"/>'),
  restart: () => svg('<path d="M4 12a8 8 0 1 0 2.4-5.7"/><path d="M4 4v4h4"/>'),
  sun: () => svg('<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/>'),
  moon: () => svg('<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"/>'),
  upload: () => svg('<path d="M12 16V4M7 9l5-5 5 5"/><path d="M4 16v3a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-3"/>'),
  arrow: () => svg('<path d="M5 12h14M13 6l6 6-6 6"/>'),
};

/** An allowed/blocked chip with an icon. */
export function chip(kind: "allowed" | "blocked", text: string): string {
  return `<span class="chip ${kind}">${kind === "allowed" ? icon.check() : icon.cross()}${text}</span>`;
}
