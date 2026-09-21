Status: v0 implementation | 2026-09-21 | implemented

# ADR-0024: `state_since` — how long an item has stood where it stands

## Context

`turn` is a pure function of `state` ([ADR-0010](ADR-0010-item-from-to-turn.md)), and `state` is
returned on every item. What no returned field answers is *how long the item has been there*.

`updated_at` looks like it should, and does not: it moves on a comment, a tag, a metadata
correction — every write, not only a transition. That is not an edge case here. ADR-0010's
2026-09-09 update established that narrating progress in a comment is the normal way a party
speaks without handing the item back, so the one axis a caller has is reset by exactly the
activity that accompanies a long wait. `order=asc` ([ADR-0020](ADR-0020-list-search-order-parameter.md))
sorts by that same field, so it inherits the same blind spot: it finds the least-recently-*touched*
item, not the longest-*standing* one.

A consumer reporting this had items sitting in `resolved` — `turn = requester`, waiting on its own
approval — for over two weeks. Nothing said so. The rows were returned by `list_items(mine=…)` the
whole time; the axis that would have made the wait visible did not exist, so the backlog was
visible only to someone who already suspected it and went looking. This is squarely inside
`docket-core`'s layer: the store owns the transition log that holds the answer, and every consumer
of a queue hits the same wall the moment it asks "what has been standing too long", which is the
question a work queue exists to make answerable.

## Options considered and trade-offs

**What the field names:**

- **Reject — `turn_since`** (the reported ask): `turn` changes on *some* transitions, not all.
  `claim` moves `open → claimed`, and both read as the assignee's turn, so a true turn-entry
  timestamp must know each past event's resulting state. `item_events` records `kind`/`actor`, not
  the state a transition produced, so the value would have to be inferred — from the actor, say
  (a claim's actor is the assignee, a reject's the requester). That is the transition-archaeology
  ADR-0010 already rejected once, in a milder form, and it breaks outright on a self-filed item
  where both parties are the same identity.
- **Accept — `state_since`, the moment the item entered its current `state`** (adopted): exactly
  what the data supports, named for what it is. `turn` is a function of `state`, so
  `now - state_since` *is* the age of the current turn, with one documented exception: a claim
  resets it while `turn` stays `assignee`. For the reported case — an item standing in `resolved` —
  the two are identical, because `submit` is the transition that produced that state.

**Where the value comes from:**

- **Reject — a stored `items.state_since` column, written by each transition**: a second source of
  truth for a fact `state` already implies, which is the precise reason `turn` and `open` are
  derived and not stored (ADR-0010, [ADR-0012](ADR-0012-item-reject-reopen-transitions.md)). Eight
  call sites write `state` today; a ninth assignment beside each is nine chances for the two to
  drift, and nothing would report the drift. It also has no honest backfill: a migration would have
  to invent a timestamp for every item that already exists.
- **Accept — derived from `item_events`** (adopted): the append-only log already records one row per
  state change (`kind = 'transition'`) and one at creation (`kind = 'created'`), written in the same
  transaction as the `UPDATE`. `state_since` is the latest of those two kinds — one correlated
  subquery over `idx_item_events_item`, no migration, no new write path, and it cannot drift from
  the log because it *is* the log. Comments are excluded by kind, which is what separates this field
  from `updated_at`.

**Items the log does not cover:**

`item_events` began on 2026-09-09 (ADR-0010's update). An item that last transitioned before that
has neither kind of row.

- **Reject — fall back to `items.created_at` for those**: it would assert the item has stood in its
  current state since it was filed. For anything that transitioned pre-log that is false, and
  overstates the age — on exactly the oldest items, which are the ones this field exists to
  surface. ADR-0010's 2026-09-08 update settled this class already: a field must not assert
  something untrue merely to avoid returning nothing.
- **Accept — `null`** (adopted): "the log does not cover this item's last transition." It is not a
  hole in the answer so much as a coarser one — nothing has moved on this item since the log began,
  which is itself the floor of a staleness estimate — and it self-heals at the item's next
  transition, permanently. An item created after the log exists always has a `created` row, so
  `null` never appears for a never-transitioned item; `created_at` is returned for it, from that
  row's own kind rather than as a fallback.

**Filtering and sorting on it:**

- **Reject, for now — `state_since_before=<ms>` and `order=state_since`**: the report said the
  field alone is enough and the rest is client-side work, and a caller that has the value on every
  row can already sort the page it asked for. Adding a filter axis and a second sort column on
  speculation is the `sort=<column>` shape ADR-0020 declined for the same reason. See the re-open
  trigger.

## Decision

```
Item.state_since: Option<i64>       # epoch millis, like created_at/updated_at
  = MAX(created_at) over this item's item_events rows with kind IN ('created', 'transition')
  = None when the item has no such row (its last transition predates the event log)

  # Derived at read time, never stored — same treatment as `turn` and `open`, for the same
  # reason. Comments are excluded by kind, which is the whole difference from `updated_at`.

  Returned on every surface that returns an Item — get_item, list_items, search_items, and
  every transition's own return value — by construction: the three SELECTs share one column
  list and one row mapper, and create_item builds the value it just wrote.
```

No new table, column, index or migration; no change to `state`, `turn`, `updated_at`, or to what
any existing field means.

`docket-mcp`'s `ItemDto` carries it through with `#[serde(default)]`, the same older-server
convention `turn`/`open`/`tags` use, in the same release — the split deployment path
(`docket-core` immediately, `docket-mcp` by GitHub release) has produced a serve-but-cannot-call gap
before, and ADR-0020 recorded why both layers move together.

## Consequences

**Gained**: "how long has this stood on the other party's side" is a subtraction against a field
that is now on every row, for MCP and HTTP callers alike, and it is retroactively correct for every
item that has transitioned since the event log began — no backfill, nothing to opt into.

**Given up**: items whose last transition predates the event log read `null` rather than a number,
and the value resets on a claim even though `turn` does not. Both are documented rather than
papered over, and both are properties of the log, not of this field.

**Implemented** (2026-09-21, same day as this decision): `Item::state_since` (`domain.rs`), derived
by `item_columns()` in `storage.rs` — one correlated subquery now shared by all three item
`SELECT`s, which also collapses a column order that had been spelled out three times into one
place. `create_item` returns the value its own `created` event just wrote. `docket-mcp`'s `ItemDto`
carries it (`#[serde(default)]`), and the `list_items`/`search_items`/`get_item` tool descriptions
state what it answers and what `updated_at` cannot. `docket-cc`'s projection DTO is deliberately a
subset and needs no change. `docs/usage.md`: glossary row, the `Item` example, `list_items`'s row,
the prose beside the `turn` explanation, and a §9 limitation for the deliberately-absent sort and
filter. `docket-console` renders it inside the turn badge (`→ requester · 15일째`) — the console
showed only `formatRelativeTime(updated_at)`, so it had the same blind spot in the human surface
that this ADR closes in the API. Verified: 5 new `docket-core` unit tests (comment moves
`updated_at` but not this; non-transition writes including `archive_item` leave it alone; a claim
resets it while `turn` doesn't; a log-less item reads `None` and self-heals on the next transition;
`list_items`/`search_items` both carry it), 1 new `docket-mcp` end-to-end test proving the field
survives `ItemDto`'s deserialize/re-serialize round trip rather than being silently dropped there,
4 new console tests — `cargo test --workspace` 348 → 354 passed / 0 failed, `npx vitest run` 65 →
69 passed, fmt/clippy/rustdoc/doc-links (263) clean.

## Re-open trigger

If sorting or filtering by this axis turns out to be needed beyond the page a caller already has —
a consumer that must scan a queue larger than one page to find what is standing longest — revisit
`order=<column>` as ADR-0020's re-open trigger frames it, adding `state_since` alongside
`created_at` as a selectable column, rather than bolting on a one-off `state_since_before` filter.
