Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Goals

## North star

**The share of items completed without human intervention.** This is the lagging indicator that most directly inverts the bottleneck problem this project starts from (see [vision.md](vision.md)'s "Why now").

**Operational definition (relaxed)**: on the `open → claimed → resolved → closed(resolution=done)` path, an admin's "refine" (see [vision.md](vision.md) S5) counts as normal operation. Only signals that the workflow itself failed — like "force-assign" or "force-close" — count as "intervention occurred."

Measuring this requires per-item admin-intervention history to accumulate in the core, so it becomes the primary metric only after M3 (console) ([ADR-0002](decisions/ADR-0002-four-layer-architecture.md)).

## Goal tree (impact map)

```
North star: share of items completed without human intervention ↑
└─ Whose behavior needs to change: the single-owner, multi-session operator (the developer)
   └─ What changes: relying on the docket board/notifications instead of memory and copy-paste
      └─ Deliverables:
         ├─ M1 core (worker/item/claim state machine)
         ├─ M2 mcp+cc (two sessions hand an item back and forth to completion)
         ├─ M3 the console's "refine" feature (see [vision.md](vision.md) S5 — the single most important feature)
         └─ adoption-rate/burndown measurement pipeline
```

## Leading indicators + stop-loss criteria (GQM)

Completion rate has no way to be measured before M3. Until then, "is this still worth continuing?" is judged by the two leading indicators below.

| Metric | What it measures | How it's measured | Current | Target | By when |
|---|---|---|---|---|---|
| Adoption rate | Share of S1~S6 ([vision.md](vision.md)) coordination cases that go through docket | Numerator = docket items created/claimed, denominator = manually-handled cases logged by hand | 0 (not implemented) | ≥50% | 4 weeks after M3 ships |
| Issue burndown | Trend in the count of open items | Weekly `(items closed) - (items created)` | 0 | ≥0 in at least 3 of 4 weeks | 4 weeks after M3 ships |

**Stop-loss criteria**: at the 4-week observation point after M3 ships, if **coordination cases themselves fall below 1 per week**, or **burndown is negative for all 4 weeks**, the project is shelved.

Completion rate (quality) is not a stop-loss criterion — being used but producing weak output is a follow-up improvement item, not a stop-loss judgment.

## Unvalidated assumptions (3 critical ones)

| ID | Assumption | If wrong | Validation |
|---|---|---|---|
| A-1 | Async coordination is good enough | Ends up "too slow to use," like the email model — hits the stop-loss criteria immediately | Observed during real M2 use |
| A-2 | Sessions spin up often enough that abandoned items eventually get picked up | Only items that never get picked up pile up on the board | 1-week self-observation |
| A-3 | Budget/extension mechanisms actually stop runaway loops | Tokens quietly get exhausted | M1~M2 integration test |
