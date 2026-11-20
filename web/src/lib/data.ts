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

export interface LocalResults {
  machine: Machine;
  timer: { resolution_ns: number; overhead_ns: number };
}

export interface MemoryResults {
  machine: Machine;
  rows: { algorithm: string; bytes_per_key_full_window: number; limit: number }[];
}

export interface TraceResults {
  machine: Machine;
  trace: { name: string; homepage: string; requests: number; clients: number; span_secs: number };
  window_secs: number;
  limit: number;
  strategies: { strategy: string; nodes: number; error_pct: number; calls_per_1k: number }[];
}

export interface OverloadAgg {
  goodput_pct: Summary;
  p99_ms: Summary;
  shed_pct: Summary;
  timeout_pct: Summary;
}

export interface OverloadResults {
  machine: Machine;
  size: { run_secs: number; sim_seeds: number; real_seeds: number };
  workers: number;
  service_ms: number;
  deadline_ms: number;
  scenarios: {
    name: string;
    description: string;
    rows: { limiter: string; sim: OverloadAgg; real: OverloadAgg | null }[];
    series: { limiter: string; source: string; points: { t: number; offered_rps: number; ok_rps: number; possible_rps: number; limit: number | null; p99_ms: number }[] }[];
  }[];
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
