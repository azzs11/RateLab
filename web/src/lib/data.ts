// Benchmark results, copied from results/ at build time.

export interface Summary {
  n: number;
  mean: number;
  min: number;
  max: number;
  stddev: number;
}

export interface Machine {
  cpu: string;
  logical_cpus: number;
  memory_bytes: number;
  os: string;
  arch: string;
  rustc: string;
  profile: string;
  git_commit: string;
  measured_at: string;
}

export interface StrategyRow {
  strategy: string;
  scenario: string;
  nodes: number;
  error_pct: Summary;
  over_pct: Summary;
  under_pct: Summary;
  calls_per_1k: Summary;
  p99_us: Summary;
  worst_window_ratio: Summary;
}

export interface StrategyResults {
  machine: Machine;
  size: { secs: number; seeds: number; nodes: number[] };
  network: { one_way_us: number; jitter_us: number; timeout_ms: number };
  limit: number;
  keys: number;
  matrix: StrategyRow[];
  skew: { strategy: string; error_no_skew_pct: Summary; error_skewed_pct: Summary }[];
  outage: {
    strategy: string;
    policy: string;
    during_allowed_pct: Summary;
    during_over_pct: Summary;
    during_under_pct: Summary;
  }[];
  convergence: { strategy: string; rebalance_ms: Summary; never: number; under_first_second_pct: Summary }[];
}

const cache = new Map<string, Promise<unknown>>();

/** Loads `data/<name>.json`; resolves to null if it was not measured. */
export function loadResults<T>(name: string): Promise<T | null> {
  if (!cache.has(name)) {
    cache.set(
      name,
      fetch(`${import.meta.env.BASE_URL}data/${name}.json`)
        .then((r) => (r.ok ? r.json() : null))
        .catch(() => null),
    );
  }
  return cache.get(name) as Promise<T | null>;
}

export const machineLine = (m: Machine): string =>
  `${m.cpu}, ${m.logical_cpus} CPUs, ${m.os}, ${m.rustc}, commit ${m.git_commit}, ${m.measured_at.slice(0, 10)}`;
