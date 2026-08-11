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

### [B-01] SPIKE Survey similar products/frameworks
- Question: do existing tools already handle multi-agent/headless-worker coordination? How does their status-tracking and claiming approach differ from docket's?
- Timebox: 1 hour
- Output: updated table in [landscape.md](landscape.md)
- Impact: re-validates the differentiation hypothesis in [landscape.md](landscape.md)
- Prerequisite: none

### [B-02] EVAL Design a measurement method for the adoption-rate denominator
- Question: how do we record manually-coordinated cases that didn't go through docket?
- Timebox: 30 minutes
- Output: one settled measurement procedure
- Impact: whether the adoption-rate metric in [goals.md](goals.md) is actually measurable
- Prerequisite: none

### [B-06] ENABLER M1 core implementation
- Output: completion of the first slice in [roadmap.md](roadmap.md)
- Impact: the L0 gate ([quality-ramp.md](quality-ramp.md))
- Prerequisite: none

### ~~[B-10] SPIKE Check maturity of Rust MCP SDKs (`rmcp`, etc.)~~ — done
Confirmed the official SDK (`rmcp`) exists and is mature enough. mcp is also settled on Rust. → [ADR-0007](decisions/ADR-0007-language-runtime.md)

## Next

### [B-07] ENABLER M2 — mcp+cc implementation
- Output: an item completed end to end between two sessions
- Impact: the L0/L1 gate, the starting point for measuring the north star / stop-loss criteria ([goals.md](goals.md))
- Prerequisite: B-06

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

### [B-12] ENABLER Restrict M1's HTTP API's default binding during the pre-auth window
- Question: until auth (#42~43) is in place, should the core HTTP API default to localhost/private-network only — and what should that default be?
- Timebox: alongside M1 scaffolding (a lightweight decision)
- Output: resolves [open-questions.md](open-questions.md) #51, the default reflected in the M1 implementation + a note in README/AGENTS.md
- Impact: minimally hardens the "closed environment" assumption in [coverage.md](coverage.md) at the code level, consistency with the public-scope decision in [ADR-0005](decisions/ADR-0005-public-scope.md)
- Prerequisite: [B-06](backlog.md) M1 core implementation (the default binding can't be set before the server exists)

## Later

### [B-08] GATE L1 pass judgment
- Output: confirmation that S1~S6 ([vision.md](vision.md)) all run manually end to end
- Impact: [quality-ramp.md](quality-ramp.md) L1
- Prerequisite: B-07

### Remaining deferred items (appendix)
The rest of [open-questions.md](open-questions.md) — decided one by one during implementation.
