Status: v0 alignment snapshot | 2026-08-20 | settled

# ADR-0012: `reject`/`reopen` transitions, and a computed `open` field

## Context

[ADR-0003](ADR-0003-item-state-schema.md) settled the item lifecycle as a strict
one-way DAG: `open -> claimed -> resolved -> closed`, with the four admin operations
(`remove`/`merge`/`force-close`/`approve`) only ever moving an item *toward* `closed`.
There has never been an edge that moves backward.

In practice this creates two distinct failure modes, both observed on a live instance
with real cross-repo traffic:

1. **Pre-approval rework has no first-class path.** Once a worker `submit`s
   (`claimed -> resolved`), the requester's only options are `approve` (accept as-is)
   or a `force-close`/`remove` admin override (abandon it). There is no way to say
   "not yet, keep working" without leaving the state machine — in observed usage this
   was worked around entirely through `comment`s while `state` stayed frozen at
   whatever it already was.
2. **`closed` is a dead end even when the closure was premature.** One item was
   migrated into the system already `closed`/`resolution=done`, based on a stale
   snapshot of a design discussion that had not actually concluded. The real
   decision-then-implementation-then-adoption conversation that followed happened
   entirely across four `comment`s on that same, still-`closed` item — the structured
   `state` field never moved, and the only way a reader could learn the item was not
   actually finished was to read the full comment thread and notice a correction
   buried in the last one.

[ADR-0003](ADR-0003-item-state-schema.md)'s "re-open trigger" named the exact
condition for revisiting this: "a stalled item was closed by mistake and could not be
brought back." Case 2 above is that condition, observed directly.

Separately, `vision.md`'s S5 scenario ("an admin refines an ambiguous request and
sends it back into flow") already anticipated *some* form of backward motion, but
scoped it to a human, console-only "refine" operation that edits the item's content.
Nothing in this ADR is that operation — S5 remains unimplemented and out of scope
here. This ADR is narrower: a bare, reason-carrying bounce-back, callable by any
worker (the primary consumer class for this project — see `principles.md`'s
philosophy statement), not gated on a human or on editing the request itself.

A third, smaller gap surfaced while designing the above: `approve` takes no request
body at all, so a closure carries no record of who closed it. The same is true of
`remove`/`merge`/`force-close`. An item can reach its terminal state with zero
identity attached to the action that put it there — for `resolution=done` in
particular, the value that is supposed to mean "the requester confirmed this," that
is a traceability gap, not a cosmetic one.

## Options considered and trade-offs

**Shape of the backward motion:**

- **Reject — leave `closed` and `resolved` terminal for everything but the four
  existing forward admin ops**: no schema change, but does not fix either failure
  mode above; the re-open trigger this project itself set is now met, so "no change"
  is no longer a neutral choice.
- **Accept — add `reject`/`reopen` edges targeting a new `rejected`/`reopened` state
  value** (the closer match to Bugzilla's own `REOPENED` status, which
  [ADR-0003](ADR-0003-item-state-schema.md) named as this project's model): makes
  "how many items are currently in a rejected/reopened backlog" a direct `state`
  filter instead of a query over comment text. Rejected here because it reopens
  exactly the pattern [ADR-0003](ADR-0003-item-state-schema.md) closed off on
  purpose — "the state count keeps growing" as more distinctions get encoded into
  `state` — for a distinction (*why* the item is back in front of the assignee) that
  this project already has a dedicated place for: the comment log. A `rejected`
  worker-facing bucket is, workflow-position-wise, identical to `claimed` — same
  party's turn, same set of next moves — so a separate `state` value would carry
  history, not position, which is exactly what `resolution` was split out of `state`
  to avoid conflating in the first place.
- **Accept — add `reject`/`reopen` edges targeting the existing `claimed` value**
  (adopted): `state` keeps meaning only "whose turn, what can happen next"; *why* it
  got here is recorded the same way every other correction in this project is
  recorded — an atomic, reason-carrying comment. This is the same reasoning
  [ADR-0010](ADR-0010-item-from-to-turn.md) used to make `turn` computed rather than
  stored: a fact that is fully determined by other fields should not get its own
  column, or it becomes a second source of truth that can drift from the thing it
  describes.

**Whether `open`/`closed` becomes a stored field:**

- **Reject — store `open: bool` alongside `state`**: convenient to filter on, but is
  now a second source of truth for a fact `state` already fully determines
  (`state != closed`), the exact failure mode [ADR-0010](ADR-0010-item-from-to-turn.md)
  already ruled out for `turn`.
- **Accept — compute `open` from `state` at read time, never stored** (adopted): same
  treatment as `turn`. Precedent for this shape exists outside this project too — Jira
  computes a small `statusCategory` (New/Indeterminate/Done) from a much larger,
  per-project `status` value, and GitHub's issue `state` is itself the two-value
  rollup with `state_reason` carrying the "why." Both keep exactly one stored axis
  and derive the coarse view from it, which is the same shape adopted here.

**Whether the four closing operations get a caller identity:**

- **Reject — leave `approve`/`remove`/`merge`/`force-close` bodiless**: no change, but
  leaves the traceability gap in place for every one of the four ways an item can
  reach `closed`, not just the new `reject`/`reopen` edges.
- **Accept — add `author` to all four** (adopted), following the exact optionality
  `add_comment` already established: defaults to `"unknown"` if omitted, no hard
  failure. Only `reject`/`reopen` additionally require a non-empty `reason` (adopted
  earlier in this same design pass) — closure needs an identity, but only the two new
  backward edges need a mandatory explanation, since they are corrections to a
  decision someone already made.

Explicitly out of scope for this ADR (found while designing it, tracked separately):
`merge`'s `resolution=duplicate` carries no reference to the item it is a duplicate
of. That is an independent gap in the `merge` operation specifically, unrelated to
backward motion or to `open`, and folding it in here would blur this ADR's scope.

## Decision

```
open --claim(worker)-------------------------> claimed
claimed --submit(worker)---------------------> resolved
resolved --approve(author)-------------------> closed(done)
resolved --reject(author, reason)------------> claimed        # new
closed --reopen(author, reason)--------------> claimed        # new, resolution -> null
(any pre-closed state) --remove(author)------> closed(invalid)
(any pre-closed state) --merge(author)-------> closed(duplicate)
(any pre-closed state) --force-close(author)-> closed(wontfix)

open := state != closed   # computed at read time, never stored — same treatment as turn
```

No new `state` or `resolution` values. No new columns beyond what `reject`/`reopen`
need to record their comment (using the existing `item_comments` table). `author`
follows `add_comment`'s existing default-to-`"unknown"` convention; `reason` is
required and rejected as invalid if empty after trimming (the same validation
[ADR-0003](ADR-0003-item-state-schema.md)'s `topic`/`title` emptiness fix already
established for other required strings).

`turn_for` is unchanged — `claimed` already maps to `Turn::Assignee`, so both new
edges land turn back where it belongs without touching that function.

## Consequences

**Gained**: both observed failure modes get a first-class path — a resolved item can
be sent back before approval, and a closed item can be corrected after the fact —
without growing the `state` enum, without a new stored axis, and without a database
migration (`item_comments` already exists). `open` becomes directly queryable and
filterable the way a GitHub-style two-tab view needs, while remaining impossible to
drift out of sync with `state`. All four ways to reach `closed` now optionally record
who closed it.

**Given up**: "how many times has this item bounced" or "show me everything currently
sitting in a rejected/reopened backlog" is not a direct `state` filter — it requires
reading `list_comments` (or, if that need becomes common, a future computed field
derived from the comment log — not a stored counter, for the same reason `open` and
`turn` are not stored). No cap or loop-detection policy is set for repeated
reject/reopen cycles; a repeatedly-rejected item simply stays in the existing
`claimed` bucket, which the project's stall-detection work already covers without
new logic.

## Re-open trigger

If "items currently rejected/reopened" becomes a frequent, real query need that
`list_comments`-scanning genuinely cannot serve well (not just "would be more
convenient as a filter"), revisit the rejected `rejected`/`reopened`-state option
above. If reject/reopen loops turn out to need a cap or an automatic stall policy,
that is scoped to the project's existing stall-detection work, not a re-open of this
ADR.
