Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Backlog

Items with no connection (not tied to any decision, assumption, or metric) don't go here.

## Now

### [B-04] ASSUMPTION Self-observe session start-up frequency
- Question: In practice, how often is a session opened on each machine? How long do periods with "no owning session" for a given topic last?
- Timebox: 1 week (just record current habits as-is)
- Output: a per-machine log of session start-up frequency
- Impact: [open-questions.md](open-questions.md) #19 (stall threshold), [goals.md](goals.md) A-2
- Prerequisite: none, can start right now

### ~~[B-01] SPIKE Survey similar products/frameworks~~ — done
Found Swarm Protocol, Hermes Kanban, Code Conductor, Vibe Kanban — see [landscape.md](landscape.md). Differentiation hypothesis holds, no re-validation forced.

### ~~[B-02] EVAL Design a measurement method for the adoption-rate denominator~~ — done
**Procedure**: an append-only log the operator keeps by hand — one line per manually-coordinated case that could have gone through docket but didn't: `YYYY-MM-DD | one-line description | topic it would have belonged to`. Weekly denominator = line count added that week. Deliberately the simplest thing that could work (matches the top-priority quality attribute, [principles.md](principles.md)); no new tooling. This only produces numbers once the operator actually keeps the log — same nature as [B-04](backlog.md)'s self-observation, so it's the operator's habit to run, not something implementation settles further. See [goals.md](goals.md)'s Adoption rate row.

### ~~[B-06] ENABLER M1 core implementation~~ — done
`docket-core`'s domain/store/HTTP layers exist; the M1 completion criteria ([roadmap.md](roadmap.md#m1--core-first-slice)) pass both via `cargo test` and a live two-worker `curl` walkthrough (see README "Running it").

### ~~[B-10] SPIKE Check maturity of Rust MCP SDKs (`rmcp`, etc.)~~ — done
Confirmed the official SDK (`rmcp`) exists and is mature enough. mcp is also settled on Rust. → [ADR-0007](decisions/ADR-0007-language-runtime.md)

## Next

### [B-07] ENABLER M2 — mcp+cc implementation
- Output: an item completed end to end between two sessions
- Impact: the L0/L1 gate, the starting point for measuring the north star / stop-loss criteria ([goals.md](goals.md))
- Prerequisite: B-06
- **Progress**: `docket-mcp` done — an rmcp stdio server exposing register/create/list/claim/submit/approve as MCP tools, HTTP client of `docket-core` (never links it as a library). Verified with a real MCP client handshake + `tools/call`, not just Rust-level calls. `docket-cc` not started — file representation location is now settled ([ADR-0008](decisions/ADR-0008-file-representation-location.md)), so scaffolding can begin; remaining `docket-cc` open questions (#28~30, #22~23) are lower-stakes and can be decided the same way M1's separator/mcp's tool wording were — as implementation forces them.

### [B-03] ASSUMPTION Sufficiency of async coordination
- Question: in M2, are items consumed usefully without delay, or does "too late to matter anymore" happen often?
- Timebox: observed during real M2 use (no separate time needed — M2 itself is the experiment)
- Output: a count of items completed vs. items that became moot due to delay
- Impact: [goals.md](goals.md) stop-loss criteria, A-1
- Prerequisite: B-07

### [B-05] ASSUMPTION Runaway prevention for budget/extension
- Question: when items arrive back-to-back, does the extension cap actually trip?
- Timebox: one integration test during M1~M2 implementation
- Output: pass/fail on a runaway-scenario test
- Impact: [open-questions.md](open-questions.md) #35, A-3
- Prerequisite: B-06, B-07

### [B-09] GATE Build the adoption-rate/burndown measurement pipeline
- Output: an actual means of measuring the leading indicators in [goals.md](goals.md)
- Impact: whether the stop-loss criteria can be judged at all
- Prerequisite: B-02, B-07

### [B-11] EVAL Design a server uptime strategy
- Question: which machine hosts the core server, and how does coordination recover if it goes down or reboots?
- Timebox: TBD (scale it together with M4 deployment design)
- Output: resolves [open-questions.md](open-questions.md) #50, one settled uptime strategy
- Impact: the premise behind [vision.md](vision.md) S4 (cross-machine handoff) itself, the M4 deployment decision ([open-questions.md](open-questions.md) #45~48)
- Prerequisite: none (can start design discussion now, independent of M1)

### ~~[B-12] ENABLER Restrict M1's HTTP API's default binding during the pre-auth window~~ — done
Defaults to `127.0.0.1`, overridable via `DOCKET_BIND`. Resolves [open-questions.md](open-questions.md) #51 (provisional until M4 auth); documented in README "Running it".

## Later

### [B-08] GATE L1 pass judgment
- Output: confirmation that S1~S6 ([vision.md](vision.md)) all run manually end to end
- Impact: [quality-ramp.md](quality-ramp.md) L1
- Prerequisite: B-07

### Remaining deferred items (appendix)
The rest of [open-questions.md](open-questions.md) — decided one by one during implementation.
