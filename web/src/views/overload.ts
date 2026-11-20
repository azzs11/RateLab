// Instrument: overload. A backend with limited capacity behind no limit, a static
// concurrency limit, AIMD and Gradient2, all on the same requests. The model is
// sim::overload, the same code the benchmark's simulated runs use.

import { figure } from "../components/figure";
import { goodputChart, LIMITER_STYLE, type GoodputLine } from "../components/goodput";
import { lineChart, type Series } from "../components/lines";
import { masthead } from "../components/masthead";
import { afterPaint, h } from "../lib/dom";
import { fmtInt, fmtPct, pretty } from "../lib/format";
import { choose, howTo, scrub, sentence } from "../lib/inline";
import { wasm } from "../lib/wasm";

const LIMITERS = ["none", "static", "aimd", "gradient2"];
const SECS = 18;
const BUCKET_MS = 500;

const ABOUT: Record<string, string> = {
  none: "Every request goes to the backend.",
  static: "A fixed number in flight. Right only while the backend stays the size it was tuned for.",
  aimd: "Adds one while the limit is in use, cuts by a tenth on every timeout: it learns from damage.",
  gradient2: "Shrinks when recent latency rises above 1.5× its long-run average: it learns from queues forming.",
};

export default async function overload(root: HTMLElement, params: URLSearchParams) {
  root.append(
    masthead(
      "Instrument 4 of 5",
      "Overload",
      "What happens when more requests arrive than a backend can serve, with and without a limit on how many may be in progress at once.",
    ),
  );
  await afterPaint();
  const core = await wasm();
  const s = {
    scenario: ["ramp", "capacity_change", "steady"].includes(params.get("s") ?? "") ? params.get("s")! : "capacity_change",
    workers: 16,
    serviceMs: 20,
    deadlineMs: 200,
    load: 1.5,
    staticLimit: 16,
  };

  const goodput = h("div", { class: "figure-body" });
  const limits = h("div", { class: "figure-body" });
  const tbody = h("tbody", {});
  const verdict = h("p", {});

  const run = () => {
    const lines: GoodputLine[] = [];
    const limitSeries: Series[] = [];
    let workerSeries: Series | null = null;
    const rows: { name: string; goodput: number; p99: number; shed: number; timeout: number }[] = [];
    for (const name of LIMITERS) {
      const r = core.OverloadRun.run(name, s.scenario, s.workers, s.serviceMs, s.deadlineMs, s.load, SECS, s.staticLimit, 1, BUCKET_MS);
      const t = Array.from(r.t());
      const offered = r.offered();
      const ok = r.ok();
      const possible = r.possible();
      const limit = r.limit();
      lines.push({ limiter: name, points: t.map((tt, i) => ({ t: tt, offered_rps: offered[i], ok_rps: ok[i], possible_rps: possible[i] })) });
      if (name !== "none") {
        limitSeries.push({ name: `${pretty(name)} limit`, ...LIMITER_STYLE[name], points: t.map((tt, i) => [tt + BUCKET_MS / 2000, limit[i]]) });
      }
      if (!workerSeries) {
        const w = r.workers();
        workerSeries = { name: "backend workers", colour: "var(--ink)", dash: "1 2", width: 2, step: true, points: t.map((tt, i) => [tt, w[i]]) };
      }
      rows.push({ name, goodput: r.goodputPct(), p99: r.p99Ms(), shed: r.shedPct(), timeout: r.timeoutPct() });
      r.free();
    }
    goodput.replaceChildren(goodputChart(lines, "Successful answers per second for each limit"));
    limits.replaceChildren(
      lineChart([...(workerSeries ? [workerSeries] : []), ...limitSeries], {
        title: "Concurrency limit over time, against the backend's workers",
        xLabel: "seconds",
        yLabel: "requests in flight",
        height: 220,
        xMax: SECS,
      }),
    );
    const best = Math.max(...rows.map((r) => r.goodput));
    tbody.replaceChildren(
      ...rows.map((r) =>
        h(
          "tr",
          { class: r.goodput === best ? "best" : "" },
          h("th", { scope: "row" }, pretty(r.name)),
          h("td", { class: "n" }, fmtPct(r.goodput, 1)),
          h("td", { class: "n" }, r.goodput > 0 ? `${fmtInt(r.p99)} ms` : "–"),
          h("td", { class: "n" }, fmtPct(r.shed, 1)),
          h("td", { class: "n" }, fmtPct(r.timeout, 1)),
        ),
      ),
    );
    const top = rows.find((r) => r.goodput === best)!;
    const fastest = rows.filter((r) => r.timeout === 0 && r.goodput > 0).sort((a, b) => a.p99 - b.p99)[0];
    verdict.replaceChildren(
      h("strong", {}, pretty(top.name)),
      ` served the most: ${fmtPct(top.goodput, 1)} of what the backend could have answered in time.`,
      fastest && fastest.name !== top.name
        ? ` ${pretty(fastest.name)} answered fastest (slowest 1% under ${fmtInt(fastest.p99)} ms) by turning more away: the limits trade throughput for latency, and none wins both.`
        : "",
    );
  };

  const scenario = choose(
    "Scenario",
    [
      { value: "ramp", text: "ramps the load to" },
      { value: "steady", text: "holds the load at" },
      { value: "capacity_change", text: "halves, then doubles, the workers at" },
    ],
    s.scenario,
    (v) => {
      s.scenario = v;
      run();
    },
  );
  const num = (label: string, key: "workers" | "serviceMs" | "deadlineMs" | "load" | "staticLimit", min: number, max: number, step: number, format: (v: number) => string) =>
    scrub({ min, max, step, value: s[key], format, label }, (v) => {
      s[key] = v;
      run();
    }).el;

  root.append(
        h(
      "div",
      { class: "page" },
      h(
        "div",
        { class: "prose" },
        sentence(
          "A backend with ",
          num("Workers", "workers", 2, 64, 1, (v) => `${v} workers`),
          " and a ",
          num("Mean service time", "serviceMs", 5, 100, 5, (v) => `${v} ms`),
          " mean service time sits behind a ",
          num("Deadline", "deadlineMs", 50, 1000, 50, (v) => `${v} ms`),
          " deadline. The test ",
          scenario.el,
          " ",
          num("Load", "load", 0.5, 6, 0.25, (v) => `${v}×`),
          " its capacity. The static limit is ",
          num("Static limit", "staticLimit", 1, 128, 1, (v) => `${v}`),
          ".",
        ),
        howTo(),
        (() => {
          const f = figure(
            "1",
            "Answered in time, per second.",
            `Every limit gets the same ${SECS} s of requests (seed 1). The dotted line is the most the backend could have answered in time.`,
            goodput,
          );
          f.classList.add("wide");
          return f;
        })(),
        h("aside", { class: "side" }, "The backend never cancels work that missed its deadline, like many real ones. That is what turns overload into collapse."),
        verdict,
        (() => {
          const f = figure("2", "What each limit believed.", "The limit each algorithm settled on, against the workers actually there.", limits);
          f.classList.add("wide");
          return f;
        })(),
        h(
          "div",
          { class: "table-wrap" },
          h(
            "table",
            {},
            h("caption", {}, "The whole run"),
            h(
              "thead",
              {},
              h(
                "tr",
                {},
                h("th", { scope: "col" }, "Limit"),
                h("th", { scope: "col", class: "n" }, "Answered in time"),
                h("th", { scope: "col", class: "n" }, "p99"),
                h("th", { scope: "col", class: "n" }, "Turned away"),
                h("th", { scope: "col", class: "n" }, "Timed out"),
              ),
            ),
            tbody,
          ),
        ),
        h("aside", { class: "side" }, h("b", {}, "Answered in time"), " is a share of what the backend could have served; capacity is estimated from the mean service time, so it can read a little over 100%."),
        h("h2", {}, "The four limits"),
        h("dl", { class: "defs" }, ...LIMITERS.flatMap((n) => [h("dt", {}, pretty(n)), h("dd", {}, ABOUT[n])])),
        h(
          "p",
          {},
          "The same model runs in the benchmark beside the server's real ",
          h("code", {}, "ConcurrencyLimitLayer"),
          " on the same requests; the ",
          h("a", { href: "#/results" }, "results"),
          " give the gap between them.",
        ),
      ),
    ),
  );
  run();
  return () => {};
}
