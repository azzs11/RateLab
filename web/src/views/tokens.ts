// Instrument: tokens. An LLM request's cost is known only after its answer has
// streamed, so a limiter has to charge before it knows. Part 1 sends one stream of
// requests to three limiters at once, live; part 2 runs an hour of traffic through
// all three and an oracle. Everything is drl_core::tokens and sim::tokens in WASM.

import { figure } from "../components/figure";
import { lineChart, type Series } from "../components/lines";
import { masthead } from "../components/masthead";
import { loadResults } from "../lib/data";
import { afterPaint, h, html } from "../lib/dom";
import { fmtInt, fmtNum, fmtPct, pretty } from "../lib/format";
import { icon } from "../lib/icons";
import { howTo, scrub, sentence } from "../lib/inline";
import { wasm, type Core } from "../lib/wasm";

const MODES = ["estimate_only", "reserve_max", "admit_then_reconcile"] as const;
const LABEL: Record<string, string> = {
  estimate_only: "Estimate only",
  reserve_max: "Reserve the maximum",
  admit_then_reconcile: "Admit, then reconcile",
};
const HOW: Record<string, string> = {
  estimate_only: "Charges a learned estimate when the request starts, and never corrects it.",
  reserve_max: "Holds the request's maximum answer while it streams, then refunds what was not used.",
  admit_then_reconcile: "Charges the estimate, then the difference when the answer is done. A shortfall is debt.",
};
const STYLE: Record<string, { colour: string; dash: string; width: number }> = {
  estimate_only: { colour: "#E69F00", dash: "9 3 2 3", width: 2 },
  reserve_max: { colour: "#0072B2", dash: "5 3", width: 2 },
  admit_then_reconcile: { colour: "#009E73", dash: "", width: 2.5 },
};
const FIRST_TOKEN_MS = 200;
const PER_TOKEN_MS = 25;

/** A small seeded generator, so the live demo's answers can be replayed. */
function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

interface Flight {
  id: number;
  length: number;
  started: number;
  tickets: number[];
  done: boolean;
}

interface LlmRow {
  accounting: string;
  wrongly_blocked_pct: { mean: number };
  overspend_pct: { mean: number };
  recovery_mean_ms: { mean: number };
}
interface LlmResults {
  scenarios: { name: string; requests: number; rows: LlmRow[] }[];
  citation: string;
  homepage: string;
}

function liveDemo(core: Core) {
  const s = { mean: 200, max: 1024, limit: 2000 };
  let random = rng(7);
  let demos = MODES.map((m) => new core.TokenDemo(m, s.limit, s.max, 256));
  let flights: Flight[] = [];
  let nextId = 1;
  let clock = 0;
  const stats = MODES.map(() => ({ admitted: 0, refused: 0 }));

  const bars = MODES.map(() => h("div", { class: "budget-fill" }));
  const zero = () => h("div", { class: "budget-zero", "aria-hidden": "true" });
  const values = MODES.map(() => h("span", { class: "num" }, ""));
  const counts = MODES.map(() => h("span", { class: "note" }, ""));
  const tracks = MODES.map((_, i) => h("div", { class: "budget-track", role: "img" }, bars[i], zero()));
  const stream = h("ol", { class: "stream", "aria-label": "Answers streaming now" });
  const live = h("p", { class: "visually-hidden", "aria-live": "polite" });

  const answerLength = () => {
    // Pareto with shape 2: mean `s.mean`, mostly short, sometimes very long.
    const u = Math.max(1e-9, random());
    return Math.max(1, Math.min(s.max, Math.round(s.mean / (2 * Math.sqrt(u)))));
  };

  const send = (n: number) => {
    const outcomes: string[] = [];
    for (let k = 0; k < n; k++) {
      const length = answerLength();
      const tickets = demos.map((d, i) => {
        const t = d.admit(300);
        if (t >= 0) stats[i].admitted++;
        else stats[i].refused++;
        return t;
      });
      flights.push({ id: nextId++, length, started: clock, tickets, done: false });
      outcomes.push(
        `Request ${nextId - 1}: ${MODES.map((m, i) => `${LABEL[m]} ${tickets[i] >= 0 ? "admitted" : "refused"}`).join(", ")}.`,
      );
    }
    live.textContent = outcomes.join(" ");
    paint();
  };

  const paint = () => {
    MODES.forEach((m, i) => {
      const d = demos[i];
      const b = d.balance();
      const share = Math.max(-1, Math.min(1, b / s.limit));
      const fill = bars[i];
      fill.style.left = share >= 0 ? "50%" : `${50 + share * 50}%`;
      fill.style.width = `${Math.abs(share) * 50}%`;
      fill.classList.toggle("debt", b < 0);
      values[i].replaceChildren(
        b < 0 ? h("span", { class: "chip blocked" }, html(icon.cross()), `owes ${fmtInt(-b)}`) : `${fmtInt(b)} left`,
      );
      tracks[i].setAttribute(
        "aria-label",
        b < 0 ? `${LABEL[m]}: owes ${fmtInt(-b)} output tokens` : `${LABEL[m]}: ${fmtInt(b)} of ${fmtInt(s.limit)} output tokens left`,
      );
      counts[i].textContent = `${stats[i].admitted} admitted, ${stats[i].refused} refused · estimate ${d.estimate()}`;
    });
    stream.replaceChildren(
      ...flights
        .filter((f) => !f.done || clock - f.started < FIRST_TOKEN_MS + f.length * PER_TOKEN_MS + 1500)
        .slice(-6)
        .map((f) => {
          const produced = Math.max(0, Math.min(f.length, Math.floor((clock - f.started - FIRST_TOKEN_MS) / PER_TOKEN_MS)));
          const pct = (produced / s.max) * 100;
          return h(
            "li",
            {},
            h("span", { class: "num" }, `#${f.id}`),
            h("span", { class: "stream-bar", "aria-hidden": "true" }, h("span", { style: `width:${pct}%` })),
            h("span", { class: "num" }, f.done ? `${f.length} tokens, done` : `${produced} tokens…`),
          );
        }),
    );
  };

  let last = performance.now();
  let frame = 0;
  const tick = (now: number) => {
    const dt = Math.min(250, now - last);
    last = now;
    clock += dt;
    demos.forEach((d) => d.advance(dt));
    for (const f of flights) {
      if (f.done) continue;
      if (clock - f.started >= FIRST_TOKEN_MS + f.length * PER_TOKEN_MS) {
        f.done = true;
        f.tickets.forEach((t, i) => {
          if (t >= 0) demos[i].complete(t, f.length);
        });
      }
    }
    flights = flights.filter((f) => !f.done || clock - f.started < FIRST_TOKEN_MS + f.length * PER_TOKEN_MS + 1500);
    paint();
    frame = requestAnimationFrame(tick);
  };
  frame = requestAnimationFrame(tick);

  const reset = () => {
    demos.forEach((d) => d.free());
    demos = MODES.map((m) => new core.TokenDemo(m, s.limit, s.max, 256));
    flights = [];
    random = rng(7);
    stats.forEach((x) => {
      x.admitted = 0;
      x.refused = 0;
    });
    paint();
  };

  const controls = sentence(
    "Answers average ",
    scrub({ min: 20, max: 600, step: 10, value: s.mean, format: (v) => `${v}`, label: "Mean answer, tokens" }, (v) => {
      s.mean = v;
    }).el,
    " tokens (most short, a few very long) up to a maximum of ",
    scrub({ min: 128, max: 4096, step: 128, value: s.max, format: (v) => fmtInt(v), label: "Maximum answer, tokens" }, (v) => {
      s.max = v;
      reset();
    }).el,
    ", and the budget is ",
    scrub({ min: 200, max: 20000, step: 200, value: s.limit, format: (v) => fmtInt(v), label: "Output tokens per minute" }, (v) => {
      s.limit = v;
      reset();
    }).el,
    " output tokens a minute. Every request goes to all three limiters.",
  );

  const el = h(
    "div",
    { class: "token-demo" },
    h(
      "div",
      { class: "row" },
      h("button", { class: "btn solid", type: "button", onclick: () => send(1) }, "Send a request"),
      h("button", { class: "btn", type: "button", onclick: () => send(5) }, "Send five at once"),
      h("button", { class: "btn ghost", type: "button", onclick: reset }, "Start again"),
    ),
    h(
      "div",
      { class: "budgets" },
      ...MODES.map((m, i) =>
        h(
          "div",
          { class: "budget" },
          h("div", { class: "budget-head" }, h("strong", {}, LABEL[m]), values[i]),
          tracks[i],
          h("div", { class: "budget-foot" }, counts[i]),
        ),
      ),
    ),
    h("div", { class: "stream-wrap" }, h("p", { class: "label" }, "Answers streaming"), stream),
    live,
  );
  paint();
  return {
    controls,
    el,
    dispose: () => {
      cancelAnimationFrame(frame);
      demos.forEach((d) => d.free());
    },
  };
}

function hourView(core: Core) {
  const s = { tenants: 20, rate: 8, mean: 200, max: 1024, limit: 12000, prior: 256 };
  const tbody = h("tbody", {});
  const chart = h("div", { class: "figure-body" });
  const verdict = h("p", {});
  const runIt = () => {
    const v = core.TokenView.run(s.tenants, s.rate, 3600, s.mean, s.max, s.limit, s.prior, 1);
    const names = v.names();
    const blocked = v.blocked();
    const wrongly = v.wrongly();
    const over = v.overspend();
    const worst = v.worst();
    const debts = v.debts();
    const rec = v.recoveryMs();
    tbody.replaceChildren(
      h(
        "tr",
        {},
        h("th", { scope: "row" }, "Oracle (knows every answer)"),
        h("td", { class: "n" }, fmtPct(v.oracleBlocked(), 1)),
        h("td", { class: "n" }, "–"),
        h("td", { class: "n" }, "0.00%"),
        h("td", { class: "n" }, "–"),
        h("td", { class: "n" }, "0"),
      ),
      ...names.map((n: string, i: number) =>
        h(
          "tr",
          {},
          h("th", { scope: "row" }, LABEL[n] ?? pretty(n)),
          h("td", { class: "n" }, fmtPct(blocked[i], 1)),
          h("td", { class: "n" }, fmtPct(wrongly[i], 1)),
          h("td", { class: "n" }, fmtPct(over[i], 2)),
          h("td", { class: "n" }, `${fmtNum(worst[i], 2)}×`),
          h("td", { class: "n" }, debts[i] > 0 ? `${fmtInt(debts[i])} (${fmtInt(rec[i])} ms)` : "0"),
        ),
      ),
    );
    const series: Series[] = [];
    names.forEach((n: string, i: number) => {
      const t = v.balanceT(i);
      const b = v.balanceV(i);
      const pts: [number, number][] = [];
      for (let k = 0; k < t.length && t[k] <= 600; k++) pts.push([t[k], b[k]]);
      series.push({ name: LABEL[n] ?? n, ...STYLE[n], points: pts });
    });
    chart.replaceChildren(
      lineChart(series, {
        title: "The busiest tenant's output budget over the first ten minutes",
        xLabel: "seconds",
        yLabel: "output tokens left",
        height: 260,
        xMax: 600,
      }),
    );
    const res = names.indexOf("reserve_max");
    const rc = names.indexOf("admit_then_reconcile");
    const est = names.indexOf("estimate_only");
    const turned = [est, res, rc].map((k) => ({ name: names[k], w: wrongly[k] })).sort((x, y) => x.w - y.w);
    verdict.replaceChildren(
      "Against the oracle, reserving the maximum turned away ",
      h("strong", {}, fmtPct(wrongly[res], 1)),
      " of all requests, admit-then-reconcile ",
      h("strong", {}, fmtPct(wrongly[rc], 1)),
      ` and estimate-only ${fmtPct(wrongly[est], 1)}. ${LABEL[turned[0].name]} turned away the fewest here. Only reserving the maximum can never overspend; admit-then-reconcile overspent ${fmtPct(over[rc], 2)} and estimate-only ${fmtPct(over[est], 2)}.`,
    );
    v.free();
  };
  const re = (key: keyof typeof s) => (v: number) => {
    s[key] = v;
    runIt();
  };
  const controls = sentence(
    scrub({ min: 2, max: 50, step: 1, value: s.tenants, format: (v) => `${v}`, label: "Tenants" }, re("tenants")).el,
    " tenants, a few busy and many quiet, send ",
    scrub({ min: 1, max: 40, step: 1, value: s.rate, format: (v) => `${v}`, label: "Requests per second" }, re("rate")).el,
    " requests a second for an hour. Answers average ",
    scrub({ min: 20, max: 600, step: 10, value: s.mean, format: (v) => `${v}`, label: "Mean answer, tokens" }, re("mean")).el,
    " tokens, up to ",
    scrub({ min: 128, max: 4096, step: 128, value: s.max, format: (v) => fmtInt(v), label: "Maximum answer, tokens" }, re("max")).el,
    "; each tenant may receive ",
    scrub({ min: 1000, max: 100000, step: 1000, value: s.limit, format: (v) => fmtInt(v), label: "Output tokens per minute" }, re("limit")).el,
    " output tokens a minute, and the first estimate is ",
    scrub({ min: 16, max: 2048, step: 16, value: s.prior, format: (v) => `${v}`, label: "First estimate, tokens" }, re("prior")).el,
    " tokens.",
  );
  runIt();
  const table = h(
    "div",
    { class: "table-wrap" },
    h(
      "table",
      {},
      h("caption", {}, "An hour of traffic, simulated in your browser"),
      h(
        "thead",
        {},
        h(
          "tr",
          {},
          h("th", { scope: "col" }, "Charging"),
          h("th", { scope: "col", class: "n" }, "Refused"),
          h("th", { scope: "col", class: "n" }, "Wrongly refused"),
          h("th", { scope: "col", class: "n" }, "Overspend"),
          h("th", { scope: "col", class: "n" }, "Worst minute"),
          h("th", { scope: "col", class: "n" }, "Debts (mean repaid in)"),
        ),
      ),
      tbody,
    ),
  );
  return { controls, table, chart, verdict };
}

export default async function tokens(root: HTMLElement) {
  root.append(
    masthead(
      "Instrument 5 of 6",
      "Tokens",
      "An LLM request costs its prompt, known when it arrives, and its answer, known only once the answer has finished streaming. A limiter has to decide before it knows. Three ways to do it, watched request by request and over an hour.",
    ),
  );
  await afterPaint();
  const core = await wasm();
  const demo = liveDemo(core);
  const hour = hourView(core);
  const real = h("p", {});
  void loadResults<LlmResults>("llm").then((l) => {
    const a = l?.scenarios.find((x) => x.name === "azure_2023");
    if (!l || !a) return;
    const row = (n: string) => a.rows.find((r) => r.accounting === n);
    const res = row("reserve_max");
    const rec = row("admit_then_reconcile");
    if (!res || !rec) return;
    real.replaceChildren(
      `On ${fmtInt(a.requests)} real requests from Azure's public LLM trace, the benchmark found the same shape: reserving the maximum turned away ${fmtPct(res.wrongly_blocked_pct.mean, 1)} of requests the oracle would have served, admit-then-reconcile ${fmtPct(rec.wrongly_blocked_pct.mean, 1)}, with debts repaid in ${fmtInt(rec.recovery_mean_ms.mean)} ms on average. Details are in the `,
      h("a", { href: "#/results" }, "results"),
      ".",
    );
  });

  const fig1 = figure(
    "1",
    "One stream of requests, three limiters.",
    `The bars show each limiter's output budget for one tenant: right of the centre line is budget left, left of it is debt. Answers stream at ${PER_TOKEN_MS} ms a token after ${FIRST_TOKEN_MS} ms.`,
    h("div", { class: "figure-body" }, demo.el),
  );
  fig1.classList.add("wide");
  const fig2 = figure(
    "2",
    "An hour, in a few milliseconds.",
    "The oracle knows every answer's length when the request arrives: what a perfect limiter would admit.",
    hour.chart,
  );
  fig2.classList.add("wide");

  root.append(
    h(
      "div",
      { class: "page" },
      h(
        "div",
        { class: "prose" },
        h("h2", {}, "Request by request"),
        demo.controls,
        howTo(),
        h("dl", { class: "defs" }, ...MODES.flatMap((m) => [h("dt", {}, LABEL[m]), h("dd", {}, HOW[m])])),
        fig1,
        h(
          "aside",
          { class: "side" },
          "Watch the bars when an answer finishes. Estimate-only charges nothing more; reserve-the-maximum refunds what was not used; admit-then-reconcile settles the difference, and drops below zero if the answers ran long.",
        ),
        h("h2", {}, "Over an hour"),
        hour.controls,
        h("div", { class: "wide" }, hour.table),
        h(
          "aside",
          { class: "side" },
          h("b", {}, "Wrongly refused"),
          " is how many fewer requests a mode admitted than the oracle. ",
          h("b", {}, "Overspend"),
          " is output served beyond each tenant's budget for the hour, as a share of all output served. ",
          h("b", {}, "Worst minute"),
          " is the most output one tenant got in a minute, as a multiple of its limit.",
        ),
        hour.verdict,
        fig2,
        real,
      ),
    ),
  );
  return () => demo.dispose();
}
