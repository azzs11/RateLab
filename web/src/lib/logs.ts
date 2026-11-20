// Parses access logs in the browser: CSV, JSON Lines, and nginx/Apache "combined".
// Nothing is uploaded anywhere.

export type Format = "csv" | "jsonl" | "combined";

export interface Table {
  format: Format;
  columns: string[];
  rows: string[][];
}

export interface Parsed {
  atMs: Float64Array;
  keys: Uint32Array;
  costs: Uint32Array;
  keyNames: string[];
  skipped: number;
}

export const MAX_ROWS = 500_000;

const TIME_NAMES = ["timestamp", "time", "ts", "@timestamp", "date", "datetime", "time_local", "request_time"];
const KEY_NAMES = ["api_key", "apikey", "key", "user_id", "userid", "client_id", "client", "tenant", "ip", "remote_addr", "client_ip", "user"];
const COST_NAMES = ["cost", "tokens", "weight", "units"];

const COMBINED =
  /^(\S+) \S+ (\S+) \[([^\]]+)\] "([^"]*)" (\d{3}) (\S+)(?: "([^"]*)" "([^"]*)")?/;

const MONTHS: Record<string, number> = {
  Jan: 0, Feb: 1, Mar: 2, Apr: 3, May: 4, Jun: 5, Jul: 6, Aug: 7, Sep: 8, Oct: 9, Nov: 10, Dec: 11,
};

/** nginx/Apache time: 10/Oct/2000:13:55:36 -0700 */
function parseClfTime(s: string): number {
  const m = /^(\d{2})\/(\w{3})\/(\d{4}):(\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?\s*([+-]\d{4})?$/.exec(s);
  if (!m) return NaN;
  const [, d, mon, y, hh, mm, ss, frac, tz] = m;
  const month = MONTHS[mon];
  if (month === undefined) return NaN;
  let t = Date.UTC(+y, month, +d, +hh, +mm, +ss) + (frac ? Number(`0.${frac}`) * 1000 : 0);
  if (tz) {
    const sign = tz[0] === "-" ? -1 : 1;
    t -= sign * (Number(tz.slice(1, 3)) * 60 + Number(tz.slice(3, 5))) * 60_000;
  }
  return t;
}

/** Any timestamp to ms: epoch seconds, ms, µs or ns, ISO 8601, or CLF. */
export function parseTime(raw: string): number {
  const s = raw.trim().replace(/^"|"$/g, "");
  if (s === "") return NaN;
  if (/^-?\d+(\.\d+)?$/.test(s)) {
    const n = Number(s);
    if (n < 1e11) return n * 1000; // seconds
    if (n < 1e14) return n; // milliseconds
    if (n < 1e17) return n / 1000; // microseconds
    return n / 1e6; // nanoseconds
  }
  const clf = parseClfTime(s);
  if (!Number.isNaN(clf)) return clf;
  return Date.parse(s);
}

function splitCsvLine(line: string, sep: string): string[] {
  const out: string[] = [];
  let cur = "";
  let quoted = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (quoted) {
      if (c === '"' && line[i + 1] === '"') {
        cur += '"';
        i++;
      } else if (c === '"') quoted = false;
      else cur += c;
    } else if (c === '"') quoted = true;
    else if (c === sep) {
      out.push(cur);
      cur = "";
    } else cur += c;
  }
  out.push(cur);
  return out.map((x) => x.trim());
}

/** Reads text into a table, detecting the format. */
export function readTable(text: string): Table {
  const lines = text.split(/\r?\n/).filter((l) => l.trim() !== "");
  if (lines.length === 0) throw new Error("The file is empty.");
  const first = lines[0].trim();
  if (first.startsWith("{")) {
    const rows: Record<string, unknown>[] = [];
    for (const l of lines.slice(0, MAX_ROWS)) {
      try {
        rows.push(JSON.parse(l));
      } catch {
        // Skipped: counted below as rows without a usable time.
        rows.push({});
      }
    }
    const columns = [...new Set(rows.slice(0, 200).flatMap((r) => Object.keys(r)))];
    return {
      format: "jsonl",
      columns,
      rows: rows.map((r) => columns.map((c) => (r[c] === undefined || r[c] === null ? "" : String(r[c])))),
    };
  }
  if (COMBINED.test(first)) {
    const columns = ["ip", "user", "time", "request", "status", "bytes", "referer", "user_agent"];
    const rows: string[][] = [];
    for (const l of lines.slice(0, MAX_ROWS)) {
      const m = COMBINED.exec(l);
      rows.push(m ? [m[1], m[2], m[3], m[4], m[5], m[6], m[7] ?? "", m[8] ?? ""] : []);
    }
    return { format: "combined", columns, rows };
  }
  const sep = [",", "\t", ";", "|"].reduce((best, s) =>
    first.split(s).length > first.split(best).length ? s : best,
  );
  const header = splitCsvLine(first, sep);
  const looksLikeHeader = header.some((h) => /[a-z_]/i.test(h) && Number.isNaN(parseTime(h)));
  const columns = looksLikeHeader ? header : header.map((_, i) => `column ${i + 1}`);
  const body = looksLikeHeader ? lines.slice(1) : lines;
  return {
    format: "csv",
    columns,
    rows: body.slice(0, MAX_ROWS).map((l) => splitCsvLine(l, sep)),
  };
}

function guess(columns: string[], names: string[]): number {
  const lower = columns.map((c) => c.toLowerCase());
  for (const n of names) {
    const i = lower.indexOf(n);
    if (i >= 0) return i;
  }
  return -1;
}

/** Best guesses for the time, key and cost columns (−1 when there is none). */
export function guessColumns(t: Table): { time: number; key: number; cost: number } {
  let time = guess(t.columns, TIME_NAMES);
  if (time < 0) {
    // The first column whose first values all parse as times.
    time = t.columns.findIndex((_, i) => t.rows.slice(0, 20).every((r) => !Number.isNaN(parseTime(r[i] ?? ""))));
  }
  let key = guess(t.columns, KEY_NAMES);
  if (key < 0) key = t.columns.findIndex((_, i) => i !== time);
  return { time, key, cost: guess(t.columns, COST_NAMES) };
}

/** Turns a table into sorted arrays for the WebAssembly replay. */
export function toArrays(t: Table, time: number, key: number, cost: number): Parsed {
  const ids = new Map<string, number>();
  const keyNames: string[] = [];
  const at: number[] = [];
  const ks: number[] = [];
  const cs: number[] = [];
  let skipped = 0;
  for (const r of t.rows) {
    const ms = parseTime(r[time] ?? "");
    if (!Number.isFinite(ms)) {
      skipped++;
      continue;
    }
    const name = key >= 0 ? (r[key] ?? "") : "all";
    let id = ids.get(name);
    if (id === undefined) {
      id = keyNames.length;
      ids.set(name, id);
      keyNames.push(name);
    }
    const c = cost >= 0 ? Math.max(1, Math.min(1_000_000, Math.round(Number(r[cost]) || 1))) : 1;
    at.push(ms);
    ks.push(id);
    cs.push(c);
  }
  return {
    atMs: Float64Array.from(at),
    keys: Uint32Array.from(ks),
    costs: Uint32Array.from(cs),
    keyNames,
    skipped,
  };
}
