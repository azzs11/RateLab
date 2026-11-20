---
title: "The Price of Exactness in Distributed Rate Limiting"
author: "Akchhya Singh"
date: "2026-11-20"
---

# The Price of Exactness in Distributed Rate Limiting

**Akchhya Singh** · 2026-11-20 · <https://github.com/azzs11/distributed-rate-limiter>

> **Abstract.** A rate limit promises at most *L* requests per window. On one machine
> the promise can be kept exactly; across a cluster it costs a round trip to shared
> state on every request. We measure that cost and what each cheaper alternative gives
> up, using one reproducible benchmark that scores every limiter against an exact
> oracle. On 5 seeds of simulated traffic over three nodes, adaptive quota
> leasing (leases that grow with use and are returned when idle) answered with
> 65% fewer store calls than a central
> exact limiter at 1.73% admission error, and the
> simulator agreed with a real three-node Redis deployment to within
> 0.12 percentage points. On a real public request log, however,
> leasing saved nothing, because most real clients send too little to reuse a lease. We
> extend the comparison to two neighbouring problems: overload, where an adaptive
> concurrency limit kept 96% of the possible goodput under a
> ramp to 3× capacity against 28% with no limit,
> and LLM traffic, where reserving each request's maximum answer turned away
> 9.2% of requests that an oracle would have served, against
> 3.9% for charging an estimate and reconciling later.

## 1. Introduction

Rate limits protect services from overload and tenants from each other. Production
limiters run on many servers at once, and the textbook algorithms assume one. A
cluster that must enforce one limit has three broad choices: ask a shared store on
every request (exact, one round trip per decision), split the limit between the nodes
(free, wrong whenever traffic is uneven), or something in between: synchronise counts
in the background, or lease chunks of quota from the store and spend them locally.

Each choice trades accuracy for traffic to the store, and that trade is usually
described rather than measured. This work measures it. Its contributions are:

1. **One benchmark for all of them.** Five single-node algorithms and five coordination
   strategies run against the same traffic and are scored against an exact oracle
   (Section 3), with every number in this report generated from the benchmark's result
   files.
2. **Adaptive quota leasing**, a leasing protocol whose lease size follows each node's
   demand like a TCP window, measured against the alternatives, including where it
   loses (Section 4.3).
3. **A simulator checked against a real cluster**, so that its results can be trusted
   for configurations a laptop cannot run (Section 4.4).
4. **Two neighbouring problems on the same footing:** adaptive concurrency limits under
   overload (Section 5) and token budgets for LLM traffic, whose cost is known only
   after the answer has streamed (Section 6), the latter on a real production trace.

## 2. Background

**Algorithms.** A *fixed window* counts requests per aligned window and resets at every
edge. A *sliding window log* keeps every admitted request of the last window and is
exact. A *token bucket* (equivalently a *leaky bucket*) holds up to *L* tokens and
refills *L* per window; *GCRA* makes the token bucket's decisions with one timestamp
per key. The fixed window's flaw is well known and easy to reproduce: *L* requests just
before an edge and *L* just after both fit, and the benchmark measured
2.00× the limit admitted within one second. The buckets have a
gentler version of the same property by design: a full bucket admits a burst on top of
a window of refill, and on steady Poisson traffic they admitted 21.0%
more than the exact rule.

**Coordination strategies.** *Central exact* asks the store (Redis, in a Lua script
using the store's clock) on every request. *Local split* gives each of *N* nodes *L/N*
and never talks. *Local with async sync* decides locally and reports counts in the
background, in the style Cloudflare has described for its edge. *Fixed quota leasing*
borrows fixed chunks of the limit from the store and spends them locally. *Adaptive
quota leasing* doubles a node's lease when it is used up quickly, halves it when the
store grants less than asked, and returns leftovers when the node goes idle, so busy
nodes borrow more and idle nodes do not hoard.

## 3. Method

**The oracle.** Admission error is measured against one perfectly coordinated token
bucket that sees every request of the cluster in order: units admitted above it, or
refused below it, per key and per window, as a share of its admissions. Single-node
accuracy is measured against a sliding window log, which admits exactly what the rule
allows. Neither shares code with the library.

**The simulator.** A deterministic discrete-event simulator runs *N* nodes, a store, a
network with one-way delay (250 µs plus jitter in the main runs), clock
skew and store outages. Strategies are implemented once, as I/O-free state machines,
and the same code runs in the simulator, in the real server and in the browser.

**The real cluster.** Three Axum servers and Redis run in Docker Compose on one laptop,
driven by a load generator that records every request. The recorded requests (same
times, same nodes) are then replayed through the simulator, so the two can be compared
request for request.

**Traffic.** Generated scenarios (steady Poisson, on/off bursts, heavy-tailed Pareto
gaps, a diurnal ramp, a flash crowd, a uniform and a skewed split across nodes, and the
boundary attack) run for 20 s with 10 keys at a limit of
100 per second, over 5 seeds; tables report the mean. Real
traffic comes from two public logs, fetched when the benchmark runs and never stored:
every HTTP request to NASA's Kennedy Space Center web server in July and August 1995,
and the Azure LLM inference trace of November 2023 (Patel et al., Splitwise: Efficient generative LLM inference using phase splitting, ISCA 2024).

**Machine.** Unless stated otherwise: Apple M5, 10 logical CPUs, 16 GiB RAM, macOS 27.0 (aarch64), rustc 1.100.0 (0ef1bc386 2026-11-09), release build. Every result file records its own
machine and commit.

## 4. Results

### 4.1 One node

On one node the choice of algorithm is mostly a choice of memory. The median decision
took between 38 and 53 ns for every algorithm
(1,000 keys, timed in batches because the laptop's clock ticks too coarsely
to time one call). GCRA kept 75 bytes per key against 2,144 for the
exact sliding window log, 28× less, while making the token
bucket's decisions. One shared GCRA limiter made 26 million decisions per
second on one thread; the fastest configuration measured, the fixed window on
16 threads, made 78 million.

### 4.2 The price of exactness

![Store calls per 1,000 decisions against admission error, three nodes, every traffic pattern](../../results/pareto-3-nodes.svg)

*Figure 1. Store calls against admission error for every strategy and traffic pattern
on three simulated nodes (log scales; lower left is better).*

| Strategy (3 nodes) | Error, steady % | Error, skewed % | Store calls / 1k, steady | Store calls / 1k, skewed |
|---|---:|---:|---:|---:|
| Central exact | 0.01 | 0.01 | 1000 | 1000 |
| Local split | 1.40 | 28.70 | 0 | 0 |
| Local + async sync | 4.56 | 1.80 | 482 | 426 |
| Fixed leasing | 2.16 | 3.29 | 306 | 213 |
| Adaptive leasing | 1.73 | 2.59 | 354 | 236 |

*Table 1. Admission error and store calls on three simulated nodes (5 seeds).*

The extremes behave as expected. Central exact is exact and pays one store call per
decision. Local split never calls the store and is accurate only while traffic is even:
when one node received most of the traffic its error reached
28.70%. Between them, leasing used
69% (fixed) and
65% (adaptive) fewer store calls than
central exact on steady traffic, at a few percent of admission error, and stayed close
to that under skew, where local split broke down.

Two robustness results matter in practice. With node clocks skewed by up to ±50 ms, a central
limiter that trusted the nodes' clocks (as an application-side Lua script would) reached
90% admission error, while every strategy that uses the store's
clock was unaffected; adaptive leasing stayed at 1.73%. After all
traffic moved from one node to another, adaptive leasing took 380
ms on average to rebalance, against 320 ms for fixed leasing.

### 4.3 Where adaptive leasing loses

The benchmark lists every configuration in which another strategy had both fewer (or
equal) store calls and less (or equal) admission error than adaptive leasing. That
happened in 18 of 21 configurations, and in
7 of the multi-node ones; most of the rest are single-node
runs, where splitting the limit is exact and free. Against fixed leasing, over the
14 multi-node configurations, adaptive leasing used fewer store
calls in 4 and had less admission error in
7. Adaptive leasing is a better default than a fixed
lease size, not a free lunch.

### 4.4 The simulator, checked

Every strategy also ran on 3 real Axum nodes sharing a real Redis,
over 3 seeds of uniform and skewed traffic, and the simulator replayed
the same recorded requests. The largest difference between them was
0.12 percentage points of admission error and
1.0% in store calls. On the same cluster, sharing one Redis
token bucket admitted 1.93% more than one perfectly coordinated bucket,
with a p99 of 1.37 ms at the client, and served 122,125 requests
per second closed-loop.

### 4.5 Real traffic

The NASA log holds 3,461,612 requests from 137,978 clients over
61 days. With the smallest limit at which the exact rule blocks at most
1% of requests (13 per 60 s per client),
the fixed window again admitted 2.00× the limit in its worst second.
The surprise was leasing: on three nodes adaptive leasing made
1055 store calls per 1,000 decisions, against
1000 for central exact. Most real clients send a few
requests and leave, so a lease is rarely reused before it expires; generated traffic,
with a handful of busy keys, had hidden this. Local split, meanwhile, reached
14.83% error on eight nodes. Real traffic also exposed a
performance bug: two strategies did work for every client ever seen on every tick,
which is harmless with ten keys and fatal with a hundred thousand.

## 5. Overload

A rate limit protects a service from a client; it does not protect a service from
itself. When a backend slows down, a rate limit keeps admitting the same rate, queues
grow past the clients' deadlines, and a backend that does not cancel late work spends
its capacity on answers nobody is waiting for. A concurrency limit caps requests in
flight instead and refuses the excess at once.

We put a backend with 16 workers and a mean service time of
20 ms (800 requests per second) behind a 200 ms
deadline and compared no limit, a static limit equal to the healthy worker count, AIMD
and Gradient2 (following Netflix's concurrency-limits), on the server's real Tower layer
and in the simulator with the same requests.

![Successful answers per second during a load ramp](../../results/overload-ramp.svg)

*Figure 2. Answers within the deadline per second as load ramps to
3× capacity.*

| Limit | Ramp: goodput % | Ramp: p99 ms | Capacity change: goodput % | Capacity change: p99 ms |
|---|---:|---:|---:|---:|
| None | 28 | 193 | 4 | 198 |
| Static | 92 | 56 | 75 | 66 |
| AIMD | 67 | 199 | 78 | 196 |
| Gradient2 | 96 | 76 | 96 | 97 |

*Table 2. Goodput as a share of what the backend could have served, and p99 latency of
the successful answers, on the real layer. The capacity change halves the workers, then
doubles them.*

With no limit, 85% of requests timed out during the ramp. A
static limit works while the backend stays the size it was tuned for, and cannot use
added capacity. AIMD learns only from timeouts, so its slowest answers sat near the
deadline. Gradient2, which reacts when recent latency rises above its long-term
average, served the most in both scenarios; the static limit answered faster by turning
more away. The real layer and the simulator differed by at most 4.6 points of
goodput.

## 6. LLM traffic: paying for answers not yet written

An LLM request costs its prompt, known on arrival, and its answer, known only when the
last token has streamed. A limiter must decide before then. We compare three ways to
charge the output: *estimate-only* charges a learned estimate and never corrects it;
*reserve-max* holds the request's maximum answer and refunds the rest; *admit then
reconcile* charges the estimate and settles the difference when the stream ends, letting
the tenant run into debt that the refill repays. An oracle that knows every answer's
length sets the bar.

On 28,185 real requests (59 minutes of the Azure trace, spread
over 20 tenants by a seeded draw because the trace has no user ids), with
limits at which the oracle refuses 5.0%:

| Charging | Refused % | Turned away vs oracle % | Overspend % | Debts (mean repaid in) |
|---|---:|---:|---:|---:|
| Estimate only | 7.8 | 2.8 | 0.34 | – |
| Reserve the maximum | 14.2 | 9.2 | 0.00 | – |
| Admit, then reconcile | 9.0 | 3.9 | 0.00 | 347 (766 ms) |

*Table 3. Token accounting on the Azure LLM trace.*

![The busiest tenant's output budget under each accounting mode](../../results/llm-balance.svg)

*Figure 3. The busiest tenant's output budget over ten minutes. Reserve-the-maximum
holds budget that most answers never use; reconcile hovers at zero with brief debts.*

Reserving the maximum is the conservative choice and the costly one: most answers are
far shorter than their maximum, so it turned away 9.2% of requests
the oracle would have served. Estimate-only overspent less than expected,
0.34%, because a learned average errs in both directions and
over an hour the errors cancel. Admit-then-reconcile kept overspend at
0.00% and turned away 3.9%,
with debts repaid in 766 ms on average. The same
requests through the server's real token layer, streaming in real time, matched the
simulator to within 0.3 points of refusals.

## 7. Limitations

The cluster shares one laptop and Docker's network, so absolute latencies describe the
protocol, not a deployment. The simulated network is a model (fixed delay plus jitter),
checked against the real cluster only at three nodes. The NASA log is from 1995 and has
one-second timestamps; the Azure trace has no user ids and records token counts, not
timings, so answers stream at an assumed speed. The overload backend is a worker-pool
model. Every result names the machine and commit it was measured on, and one command
(`just results`) reproduces all of them.

## 8. Related work

Envoy separates local limits from a global rate limit service queried over gRPC.
Cloudflare has described counting locally and synchronising asynchronously at its edge.
The Rust `governor` crate implements GCRA efficiently on one node. Netflix's
concurrency-limits library introduced the AIMD and Gradient2 limits measured here, and
Envoy's AI gateway charges LLM requests by tokens per model. The IETF RateLimit header
draft standardises how limits are reported to clients, which the server implements.
This work's contribution is not a new algorithm in any of these families but a single,
reproducible measurement across them, and an adaptive leasing protocol evaluated
honestly against its alternatives.

## 9. Conclusion

Exactness in distributed rate limiting has a price, and it can be measured: one store
call per decision buys an exact limit; quota leasing returns most of that cost for a
few percent of error on traffic dominated by busy clients, and returns nothing on
traffic dominated by sparse ones. The right strategy depends on the traffic, which is
why the benchmark ships with a log replay that measures every strategy on yours. The
same discipline applies beyond request counts: under overload an adaptive concurrency
limit keeps goodput where a fixed rate collapses, and for LLM traffic, charging an
estimate and reconciling afterwards serves far more requests than reserving the
worst case.

**Reproducing.** Code, data pipeline and results: <https://github.com/azzs11/distributed-rate-limiter>.
The library is published as `ratelab` on crates.io, and an interactive version of every
figure runs in the browser at <https://azzs11.github.io/distributed-rate-limiter/>.
