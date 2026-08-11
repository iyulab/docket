Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Quality Ramp

**Current target level: L1.**

| Level | Definition | Pass criteria | Roadmap mapping |
|---|---|---|---|
| L0 skeleton | Does it flow end to end (quality aside) | A single item actually completes `open→claimed→resolved→closed` between two sessions. **Claim exclusivity is already included here** (part of the domain definition itself — see [architecture.md](architecture.md)'s `claim` definition) | M2 |
| L1 usable | Works for the typical case | Scenarios S1~S6 ([vision.md](vision.md)) all work, even if only run manually | M2 |
| L2 dependable | Boundary/exception handling, recovery from failure | Reclaiming after a dead worker ([open-questions.md](open-questions.md) #18), preventing partial file writes (#25), runaway-prevention test passes (B-05) | Entering L2 comes after passing the stop-loss criteria (goals.md) |
| L3 operable | Observability, performance, security, docs | Console (M3) · auth ([open-questions.md](open-questions.md) #42~44) · adoption/burndown measurement (D-11, B-09) all in place | M3~M4 |

**One place where L1 doesn't casually skip past L2**: claim exclusivity is included in L0/L1 even though simplicity is the top-ranked quality attribute — because it's not an implementation detail of reliability, it's part of the very definition of `claim` ([architecture.md](architecture.md)). The rest of L1 (reclaiming, partial-write prevention, runaway prevention) is legitimately deferred to L2.

**Why the target level is pinned at L1**: the top-priority quality attribute is simplicity, and the stop-loss criteria in [goals.md](goals.md) decide "is this still worth continuing?" first. Investing in L2 (hardening reliability) comes after that judgment.
