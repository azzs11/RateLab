// The essay: one argument, in order, with live figures. Each section asks a question,
// lets the reader change the question with the underlined words, and answers it with
// the project's own Rust code running in the page. Numbers in the prose are computed
// (live, or read from the benchmark's result files); none are typed in by hand.

import { attackFigure, type AttackFigure } from "../components/attack";
import { figure } from "../components/figure";
import { goodputChart, type GoodputLine } from "../components/goodput";
import { paretoChart, paretoLegend } from "../components/pareto";
import {
  loadResults,
  type LocalResults,
  type MemoryResults,
  type OverloadResults,
  type StrategyResults,
  type TraceResults,
} from "../lib/data";
import { afterPaint, h } from "../lib/dom";
import { fmtInt, fmtPct, pretty } from "../lib/format";
import { choose, howTo, scrub, sentence } from "../lib/inline";
import { wasm, type Core } from "../lib/wasm";

const REPO = "https://github.com/azzs11/distributed-rate-limiter";

const ALGORITHMS: { name: string; state: string }[] = [
  { name: "token_bucket", state: "a level and a timestamp" },
  { name: "leaky_bucket", state: "a level and a timestamp" },
  { name: "fixed_window", state: "a counter and the window it counts" },
  { name: "sliding_window", state: "every admitted request in the last window" },
  { name: "gcra", state: "one timestamp" },
];

/** The decision at the heart of drl_core::Gcra::check, verbatim. */
const GCRA = `let base = (*tat).max(now);
let new_tat = base.saturating_add(increment);
let allowed = new_tat - now <= tolerance;
if allowed {
    *tat = new_tat;
}`;

const USE_RUST = `use drl_core::{Cost, RateLimitConfig, RateLimiter, TokenBucket};

let limiter = TokenBucket::new(RateLimitConfig::per_second(100)?);
let d = limiter.check("user_42", Cost::ONE);
if !d.allowed {
    println!("retry in {:?}", d.retry_after);
}`;

const USE_SERVER = `cargo run -p server --bin web-server -- \\
    --backend redis --concurrency gradient2

curl -i -H 'x-api-key: alice' localhost:3001/api/hello`;

const HEADERS = `HTTP/1.1 200 OK
ratelimit-policy: "public";q=100;w=1
ratelimit: "public";r=99;t=1
x-drl-source: redis
x-drl-concurrency-limit: 20`;

function section(id: string, n: number, title: string, ...body: (Node | null)[]): HTMLElement {
  return h(
    "section",
    { class: "section", id, "aria-labelledby": `${id}-title` },
    h(
      "div",
      { class: "page" },
      h(
        "div",
        { class: "prose" },
        h("h2", { id: `${id}-title` }, h("span", { class: "sec", "aria-hidden": "true" }, `§${n}`), title),
        ...body,
      ),
    ),
  );
}

const side = (...parts: (string | Node)[]) => h("aside", { class: "side" }, ...parts);

/** Reserves space for something computed later. */
function slot(label: string, minHeight: number): HTMLElement {
  return h("div", { class: "slot", role: "status", style: `min-height:${minHeight}px` }, h("span", { class: "slot-text" }, label));
}

// ---------- §1 The boundary attack ----------

function attackSection(core: Core | null, holder: { fig: AttackFigure | null }): HTMLElement {
  const outcome = h("p", {});
  const body = slot("Running the attack in your browser…", 300);
  const write = () => {
    const w = holder.fig?.worst();
    if (!w) return;
    const limit = limitScrub.value();
    const fixed = w.fixed_window;
    outcome.replaceChildren(
      "The fixed window let ",
      h("strong", {}, String(fixed)),
      ` through in a single second: ${(fixed / limit).toFixed(1)}× the limit. It keeps one counter per window and forgets it at the edge, so a burst on each side of the edge both fit. The sliding window let `,
      h("strong", {}, String(w.sliding_window)),
      " through and GCRA ",
      h("strong", {}, String(w.gcra)),
      ": neither ever passes the limit, because neither has an edge to forget at.",
    );
  };
  const limitScrub = scrub(
    { min: 2, max: 30, step: 1, value: 10, format: (v) => `${v}`, label: "Limit per second" },
    (v) => {
      holder.fig?.setLimit(v);
      write();
    },
  );
  if (core) {
    holder.fig = attackFigure(core, limitScrub.value());
    body.replaceWith(holder.fig.el);
  }
  const fig = figure(
    "1",
    "The boundary attack.",
    "Dots were allowed, crosses refused, each one a decision by drl-core compiled to WebAssembly and running in this page. Window edges are the vertical rules.",
    h("div", { class: "figure-body" }, holder.fig ? holder.fig.el : body),
  );
  fig.classList.add("wide");
  queueMicrotask(write);
  return section(
    "boundary",
    1,
    "Every limiter promises a limit",
    h(
      "p",
      {},
      "The obvious way to keep a rate limit is to count requests in fixed windows: one counter per second, reset at every edge. The obvious way is wrong, and the way it goes wrong fits in one figure.",
    ),
    sentence(
      "Take a limit of ",
      limitScrub.el,
      " requests per second. Send that many a few milliseconds before a window edge, and that many again just after it.",
    ),
    howTo(),
    fig,
    outcome,
  );
}

// ---------- §2 Five algorithms ----------

function algorithmsSection(core: Core | null, memory: MemoryResults | null): HTMLElement {
  const tbody = h("tbody", {});
  const caption = h("caption", {}, "");
  const LIMIT = 20;
  let load = 1.5;
  const measure = () => {
    if (!core) return;
    const traffic = core.Traffic.generate("bursty", 7, 20, LIMIT, 1000, 1, load);
    const rows = ALGORITHMS.map((a) => {
      const run = core.LabRun.run(a.name, LIMIT, 1000, traffic, 0, 1000);
      const row = { ...a, worst: run.worstWindow(), extra: run.admitted() - run.exactAdmitted() };
      run.free();
      return row;
    });
    traffic.free();
    caption.textContent = `On/off bursts at ${load}× a limit of ${LIMIT} per second, 20 s, measured just now in your browser`;
    tbody.replaceChildren(
      ...rows.map((r) => {
        const bytes = memory?.rows.find((m) => m.algorithm === r.name)?.bytes_per_key_full_window;
        return h(
          "tr",
          {},
          h("th", { scope: "row" }, h("a", { href: `#/lab?a=${r.name}` }, pretty(r.name))),
          h("td", {}, r.state),
          h("td", { class: "n" }, bytes === undefined ? "–" : fmtInt(bytes)),
          h("td", { class: `n${r.worst > LIMIT ? " over" : ""}` }, `${r.worst}`),
          h("td", { class: "n" }, r.extra === 0 ? "exact" : `${r.extra > 0 ? "+" : ""}${fmtInt(r.extra)}`),
        );
      }),
    );
  };
  const loadScrub = scrub(
    { min: 0.5, max: 4, step: 0.25, value: load, format: (v) => `${v}×`, label: "Load, times the limit" },
    (v) => {
      load = v;
      measure();
    },
  );
  measure();
  return section(
    "algorithms",
    2,
    "Five ways to count",
    h(
      "p",
      {},
      "Every algorithm answers the same question, may this request go now, and they differ in what they remember to answer it. Memory is the real trade: the sliding window is exact because it keeps a log; the others keep a number or two and accept some slack at the edges.",
    ),
    sentence("Here they are on bursty traffic at ", loadScrub.el, " the limit."),
    h(
      "div",
      { class: "table-wrap wide" },
      h(
        "table",
        {},
        caption,
        h(
          "thead",
          {},
          h(
            "tr",
            {},
            h("th", { scope: "col" }, "Algorithm"),
            h("th", { scope: "col" }, "Remembers per key"),
            h("th", { scope: "col", class: "n" }, "Bytes per key"),
            h("th", { scope: "col", class: "n" }, `Most in any 1 s (limit ${LIMIT})`),
            h("th", { scope: "col", class: "n" }, "Admitted vs exact"),
          ),
        ),
        tbody,
      ),
    ),
    side(
      h("b", {}, "Bytes per key"),
      " is measured by the benchmark with a counting allocator, with every key's window full",
      memory ? ` (limit ${memory.rows[0]?.limit ?? ""}, commit ${memory.machine.git_commit.slice(0, 7)})` : "",
      ". ",
      h("b", {}, "Exact"),
      " is the sliding window log: it admits exactly what the rule allows.",
    ),
    h(
      "p",
      {},
      "GCRA is the one worth reading. It keeps a single timestamp per key, the theoretical arrival time of the next request, and makes the token bucket's decisions with it. This is its whole decision, from ",
      h("a", { href: `${REPO}/blob/main/crates/core/src/algorithms/gcra.rs` }, h("code", {}, "gcra.rs")),
      ":",
    ),
    h("pre", {}, h("code", {}, GCRA)),
    h(
      "p",
      {},
      "Time is scaled to integers, so there is no rounding to drift. Every algorithm's state, step by step, is in the ",
      h("a", { href: "#/lab" }, "lab"),
      ".",
    ),
  );
}

// ---------- §3 A cluster ----------

function clusterSection(core: Core | null): HTMLElement {
  const tbody = h("tbody", {});
  let nodes = 3;
  let hot = 80;
  const answer = h("p", {});
  const run = () => {
    if (!core) return;
    const traffic = core.Traffic.generate("steady", 3, 10, 100, 1000, 1, 2);
    const rows = core.strategies().map((name: string) => {
      const opts = new core.ClusterOptions();
      opts.strategy = name;
      opts.nodes = nodes;
      opts.hot_pct = hot;
      const v = core.ClusterView.run(opts, traffic);
      const row = { name, error: v.overPct() + v.underPct(), calls: v.callsPer1k(), wait: v.p99Us() };
      v.free();
      opts.free();
      return row;
    });
    traffic.free();
    const best = Math.min(...rows.map((r) => r.error));
    tbody.replaceChildren(
      ...rows.map((r) =>
        h(
          "tr",
          { class: r.error === best ? "best" : "" },
          h("th", { scope: "row" }, pretty(r.name)),
          h("td", { class: "n" }, fmtPct(r.error)),
          h("td", { class: "n" }, fmtInt(r.calls)),
          h("td", { class: "n" }, r.wait < 1 ? "0" : `${fmtInt(r.wait)} µs`),
        ),
      ),
    );
    const central = rows.find((r) => r.name === "central_exact");
    const adaptive = rows.find((r) => r.name === "adaptive_leasing");
    if (central && adaptive) {
      answer.textContent = `Asking the store every time costs ${fmtInt(central.calls)} calls per 1,000 decisions. Adaptive leasing, which borrows chunks of quota and spends them locally, made ${fmtInt(adaptive.calls)} with ${fmtPct(adaptive.error)} error. That gap is the price of exactness, and the next figure measures it everywhere.`;
    }
  };
  const nodesScrub = scrub(
    { min: 2, max: 8, step: 1, value: nodes, format: (v) => `${v}`, label: "Nodes" },
    (v) => {
      nodes = v;
      run();
    },
  );
  const hotScrub = scrub(
    { min: 0, max: 100, step: 5, value: hot, format: (v) => (v === 0 ? "an even share" : `${v}%`), label: "Share of traffic on one node" },
    (v) => {
      hot = v;
      run();
    },
  );
  run();
  return section(
    "cluster",
    3,
    "One limit, many nodes",
    h(
      "p",
      {},
      "On one server a limiter is a data structure. Put it on several and it becomes a protocol: the servers share one limit, so they have to talk, and every way of talking trades accuracy against traffic to the shared store.",
    ),
    sentence(
      nodesScrub.el,
      " servers share a limit of 100 per second, and ",
      hotScrub.el,
      " of the traffic lands on one of them.",
    ),
    h(
      "div",
      { class: "table-wrap" },
      h(
        "table",
        {},
        h("caption", {}, "Five coordination strategies, simulated in your browser"),
        h(
          "thead",
          {},
          h(
            "tr",
            {},
            h("th", { scope: "col" }, "Strategy"),
            h("th", { scope: "col", class: "n" }, "Error"),
            h("th", { scope: "col", class: "n" }, "Store calls / 1k"),
            h("th", { scope: "col", class: "n" }, "p99 wait"),
          ),
        ),
        tbody,
      ),
    ),
    side(
      h("b", {}, "Error"),
      " is requests admitted above, or refused below, what one perfectly coordinated limiter would do, as a share of its admissions. The arrow marks the lowest. Break things (clock skew, a store outage) in the ",
      h("a", { href: "#/cluster" }, "cluster instrument"),
      ".",
    ),
    answer,
  );
}

// ---------- §4 The price of exactness ----------

function priceSection(data: StrategyResults | null): HTMLElement {
  let body: Node;
  if (!data) {
    body = h("p", { class: "note" }, "The strategy benchmark has not been run for this build.");
  } else {
    const points = data.matrix
      .filter((r) => r.nodes === 3)
      .map((r) => ({ strategy: r.strategy, scenario: r.scenario, calls: r.calls_per_1k.mean, error: r.error_pct.mean }));
    body = figure(
      "2",
      "The price of exactness.",
      `Five strategies on 3 nodes over every traffic pattern, ${data.size.seeds} seeds each, from the benchmark at commit ${data.machine.git_commit.slice(0, 7)}. Both axes are logarithmic. Select a point to replay that run.`,
      h(
        "div",
        { class: "figure-body" },
        paretoChart(points, {
          title: "Store calls against admission error for five strategies",
          onPick: (p) => {
            location.hash = `#/explorer?nodes=3&focus=${p.strategy}:${p.scenario}`;
          },
        }),
        paretoLegend(),
      ),
    );
    (body as HTMLElement).classList.add("wide");
  }
  return section(
    "price",
    4,
    "The price of exactness",
    h(
      "p",
      {},
      "Every strategy is a point on one trade-off: store calls per decision against admission error. Down and to the left is better, and the corner is empty. Asking the store every time is exact and costs a call per request; never asking is free and wrong under skew. Leasing lives in between, and where exactly is measured, not assumed.",
    ),
    body,
    h(
      "p",
      {},
      "The benchmark behind this chart also runs every strategy on three real servers sharing a real Redis, and replays the same recorded requests through the simulator to check it. The ",
      h("a", { href: "#/results" }, "results"),
      " list the gap between the two.",
    ),
  );
}

// ---------- §5 Overload ----------

function overloadSection(core: Core | null): HTMLElement {
  const chart = h("div", { class: "figure-body" }, slot("Simulating…", 260));
  const verdict = h("p", {});
  let load = 3;
  let limiter = "none";
  const draw = () => {
    if (!core) return;
    const lines: GoodputLine[] = [];
    const results: Record<string, { goodput: number; p99: number; timeout: number }> = {};
    for (const name of ["none", "gradient2"]) {
      const r = core.OverloadRun.run(name, "ramp", 16, 20, 200, load, 12, 16, 1, 500);
      const t = r.t();
      const offered = r.offered();
      const ok = r.ok();
      const possible = r.possible();
      if (limiter === "none" || name === "gradient2") {
        lines.push({
          limiter: name,
          points: Array.from(t, (tt, i) => ({ t: tt, offered_rps: offered[i], ok_rps: ok[i], possible_rps: possible[i] })),
        });
      }
      results[name] = { goodput: r.goodputPct(), p99: r.p99Ms(), timeout: r.timeoutPct() };
      r.free();
    }
    chart.replaceChildren(goodputChart(lines, "Successful answers per second as load rises"));
    const none = results.none;
    const g2 = results.gradient2;
    verdict.replaceChildren(
      "With no limit, ",
      h("strong", {}, fmtPct(none.timeout, 0)),
      ` of requests timed out and only ${fmtPct(none.goodput, 0)} of what the backend could have served came back in time: once the queue outgrows the deadline, the backend spends its time on answers nobody is waiting for. Gradient2 turned the excess away at once and kept `,
      h("strong", {}, fmtPct(g2.goodput, 0)),
      `, with the slowest 1% of answers under ${fmtInt(g2.p99)} ms.`,
    );
  };
  const loadScrub = scrub(
    { min: 1, max: 6, step: 0.5, value: load, format: (v) => `${v}×`, label: "Peak load, times capacity" },
    (v) => {
      load = v;
      draw();
    },
  );
  const which = choose(
    "Show",
    [
      { value: "none", text: "with no limit and with Gradient2" },
      { value: "gradient2", text: "with Gradient2 only" },
    ],
    limiter,
    (v) => {
      limiter = v;
      draw();
    },
  );
  draw();
  const fig = figure(
    "3",
    "Congestion collapse, and its cure.",
    "A backend with 16 workers and a 20 ms mean service time (800 requests per second) behind a 200 ms deadline. It never cancels late work. Simulated in your browser by the same code as the benchmark.",
    chart,
  );
  fig.classList.add("wide");
  return section(
    "overload",
    5,
    "When the backend cannot keep up",
    h(
      "p",
      {},
      "A rate limit protects a backend from a client. It does not protect a backend from itself: when the backend slows down, a rate limit keeps admitting the same rate, queues grow, and every request waits until it times out. A concurrency limit counts requests in flight instead, and an adaptive one learns the right number from latency.",
    ),
    sentence("Ramp the load up to ", loadScrub.el, " what the backend can serve, ", which.el, "."),
    fig,
    verdict,
    h(
      "p",
      {},
      "The same layer runs in the server (",
      h("code", {}, "--concurrency gradient2"),
      "). Static limits and AIMD, and a backend that loses half its workers mid-run, are in the ",
      h("a", { href: "#/overload" }, "overload instrument"),
      ".",
    ),
  );
}

// ---------- §6 What I got wrong ----------

function wrongSection(local: LocalResults | null, traces: TraceResults | null, overload: OverloadResults | null): HTMLElement {
  const items: HTMLElement[] = [];
  if (local) {
    items.push(
      h(
        "li",
        {},
        h("strong", {}, "My first latency numbers measured the clock. "),
        `Every algorithm came out at the same time per decision, which was the tick of the Mac's clock (${local.timer.resolution_ns} ns), not the algorithm. Latency is now timed over batches of calls.`,
      ),
    );
  }
  if (traces) {
    const a = traces.strategies.find((s) => s.strategy === "adaptive_leasing" && s.nodes === 3);
    const c = traces.strategies.find((s) => s.strategy === "central_exact" && s.nodes === 3);
    items.push(
      h(
        "li",
        {},
        h("strong", {}, "Real traffic broke an assumption and found a bug. "),
        `On a public log of ${fmtInt(traces.trace.requests)} requests from ${fmtInt(traces.trace.clients)} clients, two strategies did work for every client ever seen on every tick, and one run never finished. They now track only active keys. And leasing, which looked cheap on generated traffic, made ${a ? fmtInt(a.calls_per_1k) : "–"} store calls per 1,000 decisions against ${c ? fmtInt(c.calls_per_1k) : "–"} for asking every time: most real clients send too few requests to reuse a lease.`,
      ),
    );
  }
  if (overload) {
    const row = overload.scenarios.find((s) => s.name === "ramp")?.rows.find((r) => r.limiter === "aimd");
    const a = row?.real ?? row?.sim;
    if (a) {
      items.push(
        h(
          "li",
          {},
          h("strong", {}, "AIMD waits for the damage. "),
          `It only backs off after requests time out, so under overload its slowest answers sat at ${fmtInt(a.p99_ms.mean)} ms against a ${overload.deadline_ms} ms deadline. Gradient2 reacts to latency before anything times out.`,
        ),
      );
    }
  }
  items.push(
    h(
      "li",
      {},
      h("strong", {}, "Adaptive leasing does not always win. "),
      "It was the idea I most wanted to work. The results list every configuration where a simpler strategy beats it, generated from the data rather than chosen.",
    ),
  );
  return section(
    "wrong",
    6,
    "What I got wrong",
    h("p", {}, "A benchmark that never surprises its author is probably measuring the author. These are the results that changed the code or the claims."),
    h("ol", { class: "wrong-list" }, ...items),
  );
}

// ---------- §7 Use it ----------

function useSection(): HTMLElement {
  return section(
    "use",
    7,
    "Using it",
    h(
      "p",
      {},
      "The algorithms and strategies are one Rust crate with no I/O and an injected clock, so tests never sleep. Around it: a Redis store with a circuit breaker, and an Axum server that sends the IETF RateLimit headers and can shed overload.",
    ),
    h("pre", {}, h("code", {}, USE_RUST)),
    side("The clock is injected: tests use ", h("code", {}, "ManualClock"), " and advance time by hand."),
    h("pre", {}, h("code", {}, USE_SERVER)),
    h("p", {}, "Every answer says where it stands, for example:"),
    h("pre", {}, h("code", {}, HEADERS)),
    side(
      "Headers follow ",
      h("a", { href: "https://datatracker.ietf.org/doc/draft-ietf-httpapi-ratelimit-headers/" }, "draft-ietf-httpapi-ratelimit-headers"),
      ". Times round up, so a client that waits as told never retries early.",
    ),
    h("p", {}, "Source, the benchmark and the full results are on ", h("a", { href: REPO }, "GitHub"), "."),
  );
}

/** In-page links must not change the route (the router owns the hash). */
function jump(e: Event, id: string) {
  e.preventDefault();
  document.getElementById(id)?.scrollIntoView({ behavior: "smooth", block: "start" });
}

export default function home(root: HTMLElement) {
  const holder: { fig: AttackFigure | null } = { fig: null };
  let disposed = false;
  const dated = h("span", {});
  const masthead = h(
    "header",
    { class: "masthead" },
    h(
      "div",
      { class: "page" },
      h(
        "div",
        { class: "prose" },
        h("p", { class: "eyebrow" }, "An essay with live figures"),
        h(
          "nav",
          { class: "side toc", "aria-label": "Contents" },
          h(
            "ol",
            {},
            ...[
              ["boundary", "Every limiter promises a limit"],
              ["algorithms", "Five ways to count"],
              ["cluster", "One limit, many nodes"],
              ["price", "The price of exactness"],
              ["overload", "When the backend cannot keep up"],
              ["wrong", "What I got wrong"],
              ["use", "Using it"],
            ].map(([id, t]) => h("li", {}, h("a", { href: `#${id}`, onclick: (e: Event) => jump(e, id) }, t))),
          ),
        ),
        h("h1", {}, "The price of exactness in rate limiting"),
        h(
          "p",
          { class: "standfirst" },
          "A rate limit promises at most so many requests in so much time. This page tests that promise with a Rust library compiled into the page: on one machine, across servers that must agree, and under more load than a backend can take.",
        ),
        h(
          "p",
          { class: "byline" },
          h("span", {}, "By ", h("a", { href: "https://github.com/azzs11" }, "Akchhya Singh")),
          dated,
          h("a", { href: REPO }, "Source"),
        ),
      ),
    ),
  );
  root.append(masthead);
  const sections = h("div", {});
  root.append(sections);

  void afterPaint().then(() => Promise.all([
    wasm(),
    loadResults<StrategyResults>("strategies"),
    loadResults<MemoryResults>("memory"),
    loadResults<LocalResults>("local"),
    loadResults<TraceResults>("traces"),
    loadResults<OverloadResults>("overload"),
  ])).then(([core, strategies, memory, local, traces, overload]) => {
    if (disposed) return;
    if (local) dated.textContent = `Measured ${local.machine.measured_at.slice(0, 10)}`;
    // One section per task, after the first paint, so reading never waits on a figure.
    const builders = [
      () => attackSection(core, holder),
      () => algorithmsSection(core, memory),
      () => clusterSection(core),
      () => priceSection(strategies),
      () => overloadSection(core),
      () => wrongSection(local, traces, overload),
      () => useSection(),
    ];
    const next = () => {
      const build = builders.shift();
      if (!build || disposed) return;
      sections.append(build());
      setTimeout(next, 0);
    };
    requestAnimationFrame(() => setTimeout(next, 0));
  });

  return () => {
    disposed = true;
    holder.fig?.dispose();
  };
}
