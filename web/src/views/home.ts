// The story page: opens on the boundary attack, then walks from one limiter to a
// cluster to the benchmark. Every number is computed by the project's own code.

import { attackFigure } from "../components/attack";
import { figure } from "../components/figure";
import { paretoChart, paretoLegend } from "../components/pareto";
import { loadResults, type StrategyResults } from "../lib/data";
import { h, html } from "../lib/dom";
import { fmtInt, fmtPct, pretty } from "../lib/format";
import { icon } from "../lib/icons";
import { wasm, type Core } from "../lib/wasm";

const ALGORITHMS: { name: string; idea: string; glyph: string }[] = [
  { name: "token_bucket", idea: "Tokens drip into a bucket; each request spends one. Bursts up to the bucket size.", glyph: "bucket" },
  { name: "leaky_bucket", idea: "Requests pour into a bucket that drains at a steady rate. Same decisions as the token bucket, seen from the other side.", glyph: "leak" },
  { name: "fixed_window", idea: "A counter per window, reset at every edge. Cheap, and fooled at the edges.", glyph: "window" },
  { name: "sliding_window", idea: "A log of every admitted request in the last window. Exact, and memory grows with the limit.", glyph: "log" },
  { name: "gcra", idea: "One number per key: the theoretical arrival time of the next request. Token bucket decisions, one timestamp of state.", glyph: "tat" },
];

function section(id: string, num: string, title: string, lede: string, ...body: Node[]): HTMLElement {
  return h(
    "section",
    { class: "section reveal", id, "aria-labelledby": `${id}-title` },
    h(
      "div",
      { class: "page" },
      h(
        "div",
        { class: "section-head" },
        h("div", {}, h("p", { class: "num-mark" }, num), h("h2", { class: "display", id: `${id}-title` }, title)),
        h("p", { class: "lede" }, lede),
      ),
      ...body,
    ),
  );
}

function algorithmCards(core: Core): HTMLElement {
  // Measured live: the same bursty traffic through every algorithm.
  const limit = 20;
  const traffic = core.Traffic.generate("bursty", 7, 20, limit, 1000, 1, 1.5);
  const cards = ALGORITHMS.map((a) => {
    const run = core.LabRun.run(a.name, limit, 1000, traffic, 0, 1000);
    const worst = run.worstWindow();
    const extra = run.admitted() - run.exactAdmitted();
    run.free();
    return h(
      "a",
      { class: "algo-card", href: `#/lab?a=${a.name}` },
      h("span", { class: "algo-glyph", "data-glyph": a.glyph, "aria-hidden": "true" }),
      h("h3", {}, pretty(a.name)),
      h("p", {}, a.idea),
      h(
        "dl",
        { class: "algo-stats" },
        h("div", {}, h("dt", {}, "Peak / 1 s"), h("dd", { class: "num" }, `${worst} / ${limit}`)),
        h(
          "div",
          {},
          h("dt", {}, "vs exact"),
          h("dd", { class: "num" }, extra === 0 ? "exact" : `${extra > 0 ? "+" : ""}${fmtInt(extra)}`),
        ),
      ),
      h("span", { class: "algo-go" }, "Open in the lab ", html(icon.arrow())),
    );
  });
  traffic.free();
  return h(
    "div",
    {},
    h("div", { class: "algo-grid" }, ...cards),
    h(
      "p",
      { class: "note" },
      `Measured just now in your browser: 20 s of on/off bursts at 1.5× a limit of ${limit} per second. "vs exact" is how many more requests got through than the sliding window log, which admits exactly what the rule allows.`,
    ),
  );
}

function clusterTeaser(core: Core): HTMLElement {
  const traffic = core.Traffic.generate("steady", 3, 10, 100, 1000, 1, 2);
  const rows = core.strategies().map((name: string) => {
    const opts = new core.ClusterOptions();
    opts.strategy = name;
    opts.hot_pct = 80;
    const v = core.ClusterView.run(opts, traffic);
    const row = {
      name,
      error: v.overPct() + v.underPct(),
      calls: v.callsPer1k(),
      wait: v.p99Us(),
    };
    v.free();
    opts.free();
    return row;
  });
  traffic.free();
  const best = Math.min(...rows.map((r) => r.error));
  return h(
    "div",
    { class: "grid-2 cluster-teaser" },
    h(
      "div",
      { class: "stack" },
      h(
        "p",
        {},
        "One node can keep a limit exactly. Three nodes sharing one limit have to talk, and every way of talking trades accuracy for traffic to the shared store.",
      ),
      h(
        "p",
        {},
        "The table on the right was simulated in your browser a moment ago: 3 nodes, one limit of 100 per second, 80% of the traffic landing on one node. Admission error is measured against a single perfectly coordinated limiter.",
      ),
      h("a", { class: "btn", href: "#/cluster" }, "Open the cluster simulator ", html(icon.arrow())),
    ),
    h(
      "div",
      { class: "table-wrap" },
      h(
        "table",
        {},
        h("caption", { class: "visually-hidden" }, "Five coordination strategies on a skewed 3-node cluster"),
        h(
          "thead",
          {},
          h("tr", {}, h("th", { scope: "col" }, "Strategy"), h("th", { class: "n", scope: "col" }, "Error"), h("th", { class: "n", scope: "col" }, "Store calls / 1k"), h("th", { class: "n", scope: "col" }, "p99 wait")),
        ),
        h(
          "tbody",
          {},
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
        ),
      ),
    ),
  );
}

async function paretoPreview(): Promise<HTMLElement> {
  const data = await loadResults<StrategyResults>("strategies");
  if (!data) {
    return h("p", { class: "note" }, "The strategy benchmark has not been run for this build yet.");
  }
  const points = data.matrix
    .filter((r) => r.nodes === 3)
    .map((r) => ({ strategy: r.strategy, scenario: r.scenario, calls: r.calls_per_1k.mean, error: r.error_pct.mean }));
  return figure(
    "2",
    "The price of exactness",
    `3 nodes, every traffic pattern, ${data.size.seeds} seeds each, from the benchmark (commit ${data.machine.git_commit}). Lower left is better. Select a point to replay it.`,
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
}

const USE_RUST = `use drl_core::{Cost, RateLimitConfig, RateLimiter, TokenBucket};

let limiter = TokenBucket::new(RateLimitConfig::per_second(100)?);
let d = limiter.check("user_42", Cost::ONE);
if !d.allowed {
    println!("retry in {:?}", d.retry_after);
}`;

const USE_SERVER = `docker run --rm -p 6379:6379 redis:7.4-alpine
cargo run -p server --bin web-server -- --backend redis

curl -i -H 'x-api-key: alice' localhost:3001/api/hello
# RateLimit-Policy: "public";q=100;w=1
# RateLimit: "public";r=99;t=1`;

export default async function home(root: HTMLElement) {
  const core = await wasm();
  const attack = attackFigure(core);

  const hero = h(
    "section",
    { class: "hero", "aria-labelledby": "hero-title" },
    h(
      "div",
      { class: "page hero-grid" },
      h(
        "div",
        { class: "hero-copy" },
        h("p", { class: "kicker" }, "A rate limiter in Rust · a lab in your browser"),
        h(
          "h1",
          { class: "display", id: "hero-title" },
          "Every limiter promises a limit. ",
          h("span", { class: "hero-em" }, "Watch which ones keep it."),
        ),
        h(
          "p",
          { class: "lede" },
          "Ten requests just before a window edge, ten just after. The limit is ten per second. One of these three algorithms lets twenty through in a single second.",
        ),
        h(
          "div",
          { class: "row" },
          h("a", { class: "btn accent", href: "#/lab" }, "Open the lab ", html(icon.arrow())),
          h("a", { class: "btn ghost", href: "#/replay" }, html(icon.upload()), "Replay your own logs"),
        ),
      ),
      figure(
        "1",
        "The boundary attack",
        "Each mark is a decision made by the real library, compiled to WebAssembly, running in this page.",
        h("div", { class: "figure-body" }, attack.el),
      ),
    ),
  );

  const lab = section(
    "algorithms",
    "01",
    "Five ways to count",
    "Every algorithm answers the same question: may this request go now? They disagree in the details, and the details are where limits leak.",
    algorithmCards(core),
  );

  const cluster = section(
    "cluster",
    "02",
    "One limit, many nodes",
    "Put the limiter on three servers and it stops being a data structure. It becomes a protocol.",
    clusterTeaser(core),
  );

  const price = section(
    "price",
    "03",
    "The price of exactness",
    "Asking the shared store on every request is exact and slow. Never asking is free and wrong. Leasing sits in between: how far in between is measured, not guessed.",
    await paretoPreview(),
  );

  const use = section(
    "use",
    "04",
    "Use it",
    "A Rust library with an injected clock and exact integer math, an Axum server that sends the IETF RateLimit headers, and a Redis store shared by every node.",
    h(
      "div",
      { class: "grid-2" },
      h("div", { class: "stack" }, h("h3", {}, "In a Rust service"), h("pre", {}, h("code", {}, USE_RUST))),
      h("div", { class: "stack" }, h("h3", {}, "As a server, shared through Redis"), h("pre", {}, h("code", {}, USE_SERVER))),
    ),
    h(
      "p",
      { class: "note" },
      "Every number on this site is computed by the same Rust code: live in your browser, or by the reproducible benchmark (",
      h("a", { href: "#/results" }, "results"),
      ").",
    ),
  );

  root.append(hero, lab, cluster, price, use);

  return () => {
    attack.dispose();
  };
}
