// Playback clock. With prefers-reduced-motion, time advances in discrete steps
// (a few frames per second) instead of continuous animation.

export const reducedMotion = (): boolean =>
  matchMedia("(prefers-reduced-motion: reduce)").matches;

export interface Player {
  /** Current time in ms. */
  readonly t: number;
  readonly playing: boolean;
  play(): void;
  pause(): void;
  toggle(): void;
  seek(ms: number): void;
  setDuration(ms: number): void;
  setSpeed(simMsPerRealMs: number): void;
  dispose(): void;
}

/**
 * Runs `onFrame(t)` while playing. `speed` is simulation ms per real ms. Loops back to
 * the start at the end when `loop` is set.
 */
export function player(
  onFrame: (t: number) => void,
  opts: { duration: number; speed?: number; loop?: boolean; onState?: (playing: boolean) => void },
): Player {
  let duration = opts.duration;
  let t = 0;
  let playing = false;
  let raf = 0;
  let last = 0;
  let stepTimer = 0;
  let speed = opts.speed ?? 1;

  const advance = (dt: number) => {
    t += dt * speed;
    if (t >= duration) {
      if (opts.loop) t = 0;
      else {
        t = duration;
        stop();
      }
    }
    onFrame(t);
  };

  const frame = (now: number) => {
    if (!playing) return;
    const dt = last ? Math.min(now - last, 100) : 16;
    last = now;
    advance(dt);
    if (playing) raf = requestAnimationFrame(frame);
  };

  function stop() {
    playing = false;
    cancelAnimationFrame(raf);
    clearInterval(stepTimer);
    opts.onState?.(false);
  }

  const api: Player = {
    get t() {
      return t;
    },
    get playing() {
      return playing;
    },
    play() {
      if (playing) return;
      if (t >= duration) t = 0;
      playing = true;
      opts.onState?.(true);
      if (reducedMotion()) {
        // Stepped frames: 4 per second, each a quarter second of real time.
        stepTimer = window.setInterval(() => advance(250), 250);
      } else {
        last = 0;
        raf = requestAnimationFrame(frame);
      }
    },
    pause: stop,
    toggle() {
      if (playing) stop();
      else api.play();
    },
    seek(ms: number) {
      t = Math.max(0, Math.min(duration, ms));
      onFrame(t);
    },
    setDuration(ms: number) {
      duration = Math.max(1, ms);
      if (t > duration) t = duration;
    },
    setSpeed(s: number) {
      speed = s;
    },
    dispose: stop,
  };
  return api;
}

/** Index of the last element of sorted `xs` that is <= `x` (−1 if none). */
export function upperIndex(xs: ArrayLike<number>, x: number): number {
  let lo = 0;
  let hi = xs.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (xs[mid] <= x) lo = mid + 1;
    else hi = mid;
  }
  return lo - 1;
}
