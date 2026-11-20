// A crisp, resizable canvas: sized by CSS, drawn at device pixel ratio.

export interface Surface {
  canvas: HTMLCanvasElement;
  ctx: CanvasRenderingContext2D;
  width: number;
  height: number;
}

/** Creates a canvas with a fixed aspect ratio that redraws on resize. */
export function surface(
  label: string,
  aspect: number,
  draw: (s: Surface) => void,
  minHeight = 160,
): { el: HTMLElement; surface: Surface; redraw: () => void; dispose: () => void } {
  const wrap = document.createElement("div");
  wrap.className = "canvas-wrap";
  const canvas = document.createElement("canvas");
  canvas.setAttribute("role", "img");
  canvas.setAttribute("aria-label", label);
  wrap.appendChild(canvas);
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("canvas 2D is not available");
  const s: Surface = { canvas, ctx, width: 0, height: 0 };

  const size = () => {
    const w = Math.max(1, wrap.clientWidth);
    const hgt = Math.max(minHeight, Math.round(w / aspect));
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(hgt * dpr);
    canvas.style.height = `${hgt}px`;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    s.width = w;
    s.height = hgt;
  };
  const redraw = () => {
    if (s.width === 0) size();
    ctx.clearRect(0, 0, s.width, s.height);
    draw(s);
  };
  const ro = new ResizeObserver(() => {
    size();
    redraw();
  });
  ro.observe(wrap);
  return { el: wrap, surface: s, redraw, dispose: () => ro.disconnect() };
}

/** Draws an "allowed" dot (filled circle) or "blocked" mark (a cross): shape plus colour. */
export function mark(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  r: number,
  allowed: boolean,
  allowedColour: string,
  blockedColour: string,
): void {
  if (allowed) {
    ctx.fillStyle = allowedColour;
    ctx.beginPath();
    ctx.arc(x, y, r, 0, Math.PI * 2);
    ctx.fill();
  } else {
    ctx.strokeStyle = blockedColour;
    ctx.lineWidth = Math.max(1.5, r * 0.55);
    ctx.beginPath();
    ctx.moveTo(x - r, y - r);
    ctx.lineTo(x + r, y + r);
    ctx.moveTo(x + r, y - r);
    ctx.lineTo(x - r, y + r);
    ctx.stroke();
  }
}
