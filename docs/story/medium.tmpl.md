# What does an exact rate limit cost? I measured it.

*A rate limiter in Rust, a benchmark that grades it against an exact answer, and the
results that surprised me.*

Take a limit of ten requests per second. Send ten requests a few milliseconds before a
window edge, and ten more just after it. A fixed-window counter, the way most of us
first write a rate limiter, lets them all through: in my benchmark it admitted
{{attack.fixed.worst}}× the limit within one second. That one figure is the reason this
project exists. A rate limit is a promise, and I wanted to know which promises hold,
and what keeping them costs.

## The question

On one server, keeping the promise exactly is cheap. Across a cluster it is not: an
exact limit needs every server to ask shared state, usually Redis, on every request.
The alternatives are well known (split the limit between servers, count locally and
sync in the background, lease chunks of quota), and each trades accuracy for fewer
round trips. What I could not find was that trade measured, all of them on the same
traffic, against the same exact answer.

So I built [Ratelab](https://github.com/azzs11/distributed-rate-limiter): a Rust
library with five algorithms and five ways for a cluster to share one limit, and a
benchmark that scores all of them against an exact oracle.

## How I measured

Three rules kept me honest.

**Grade against an exact answer.** Every run is compared with one perfectly coordinated
limiter that sees every request in order. Admission error is what a strategy let
through above it, or refused below it.

**Check the simulator against reality.** Most results come from a deterministic
simulator, which can run thousands of configurations a laptop cannot. To trust it, I
ran every strategy on three real servers sharing a real Redis, recorded every request,
and replayed exactly those requests through the simulator. They agreed to within
{{realcheck.err_gap_pp}} percentage points of admission error.

**No number I typed by hand.** Every number in the README, the report and this post is
filled in by code from the benchmark's result files. If I re-run it on another machine,
the prose updates with it.

## What I found

**Leasing returns most of the cost.** With three servers on steady traffic, central
exact limiting made one Redis call per request. Adaptive quota leasing, my own variant
where each server's lease grows with demand and is returned when idle, made
{{s3.adaptive_leasing.steady_poisson.saved_pct}}% fewer calls at
{{s3.adaptive_leasing.steady_poisson.err}}% admission error. Splitting the limit evenly
needed no calls at all and fell apart as soon as traffic was uneven:
{{s3.local_split.skewed.err}}% error when most traffic hit one server.

**Then real traffic took the win away.** I replayed {{trace.requests}} real requests
from {{trace.clients}} clients (NASA's public web logs from 1995). On that traffic,
leasing made {{trace.adaptive_leasing.3.calls}} Redis calls per 1,000 decisions,
against {{trace.central_exact.3.calls}} for asking every time. Real clients mostly send
a few requests and leave, so a lease is rarely reused. My generated traffic, with a
handful of busy keys, had hidden that completely.

**Rate limits do not stop overload.** A rate limit protects a service from its
clients, not from itself. When a backend slows down and does not cancel late work, a
queue builds past every deadline and almost nothing comes back in time: with no limit,
a load ramp to {{ov.ramp.peak_x}}× capacity kept {{ov.ramp.none.goodput}}% of the
possible goodput. A Gradient2 concurrency limit, which backs off when latency starts
rising, kept {{ov.ramp.gradient2.goodput}}%.

**For LLM traffic, the safe choice is the expensive one.** An LLM request's cost is
known only after the answer has streamed. Reserving each request's maximum answer never
overspends, but on {{llm.requests}} real requests from Azure's public LLM trace it
turned away {{llm.reserve_max.wrong}}% of requests that a limiter knowing the answers
in advance would have served. Charging an estimate and settling the difference
afterwards turned away {{llm.admit_then_reconcile.wrong}}%, and the debts it ran into
were repaid in {{llm.admit_then_reconcile.repaid_ms}} ms on average.

## What I got wrong

My first latency numbers measured the Mac's clock, not the algorithms. Real traffic
found a bug that generated traffic never would: two strategies did work for every
client ever seen on every tick, harmless with ten keys and fatal with a hundred
thousand. And adaptive leasing, the idea I most wanted to win, does not always: the
benchmark lists every configuration where a simpler strategy beats it, and there are
{{adaptive.beaten_multinode}} of them among the multi-node runs.

## Try it

- **In the browser:** <https://azzs11.github.io/distributed-rate-limiter/>. The library
  runs there as WebAssembly; drag the numbers in the sentences.
- **In Rust:** `cargo add ratelab`.
- **On your own traffic:** `ratelab replay access.log` recommends a limit per caller.
- **The full write-up:** *The Price of Exactness in Distributed Rate Limiting*, in the
  repository.
