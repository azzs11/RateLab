# Roadmap

**Goal:** a Rust rate limiter that proves how accurate it is. It compares exact,
approximate and adaptive distributed limiting on one reproducible benchmark,
adds token-aware limits for LLM traffic, and ships a WASM playground.

## Phases

The work runs in nine phases. They are numbered by topic and done in this
order: **0 → 1 → 2 → 3 → 6 → 4 → 5 → 7 → 8**.

| # | Phase | Summary |
|---|-------|---------|
| 0 | Honesty and housekeeping | Remove unverified claims and numbers, delete dead files, fix links, make CI enforce clippy and rustfmt. |
| 1 | Workspace, thread safety, Clock | Split into a Cargo workspace, make limiters safe to share across threads, and inject a `Clock` so tests use a mock clock instead of sleeps. |
| 2 | Truly distributed + honest benchmark | Have the server actually use the Redis backend, and add one reproducible benchmark that measures accuracy as well as speed. |
| 3 | Coordination strategies + Adaptive Quota Leasing | Compare exact (central store), approximate (local plus periodic sync) and adaptive (leased quota) coordination on the same benchmark. |
| 4 | Adaptive concurrency | Limits that adjust to observed latency or errors rather than a fixed rate. |
| 5 | Token-aware LLM limits | Limit on tokens (input/output) as well as requests, for LLM API traffic. |
| 6 | Playground + UI | Compile the algorithms to WASM for an in-browser playground, and rebuild the dashboard on real, measured data. |
| 7 | Publish crate | Clean up the public API and documentation, then publish to crates.io. |
| 8 | Story / report | Write up the method and the measured results. |

## Rules

- **Plan first:** read the code, list the files to touch, then work in small steps.
- **Keep tests green.** New code gets tests, preferably with a mock clock and no sleeps.
- **No unverified numbers:** a number goes in the README, program output or UI only if code in this repo produced it.
- **One logical change per commit**, with conventional-commit messages (`feat:`, `fix:`, `docs:`, `chore:`, `test:`, `refactor:`, …).
- **Ask before** adding a paid service or a heavy dependency.
