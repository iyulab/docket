Status: v0 alignment snapshot | 2026-08-18 | settled

# ADR-0011: `from`/`to` renamed to `requester`/`assignee`

## Context

[ADR-0010](ADR-0010-item-from-to-turn.md) introduced `Item.from`/`Item.to` as the wire names for
the two-party handoff, deliberately keeping `to` as a straight rename of the pre-existing `owner`
field. In practice, two problems with that naming showed up once the console started surfacing it
as first-class UI (table columns, a per-item detail page) rather than an internal implementation
detail:

- **`to` and `topic` had converged.** The console's `to`-with-topic-fallback display (ADR-0010's
  "Given up" section) meant `to` was showing the item's `topic` for every unclaimed item — the two
  columns read as near-duplicates, and `topic` stopped earning its own place as a distinct
  dimension of the list (the table grouped by `topic` instead once this was noticed).
- **`from`/`to` are prepositions, not role names.** They read naturally as "moving from A to B," a
  direction, not "the person who filed this" vs. "the person doing it" — a role-shaped relationship
  the two-party model in ADR-0010 already describes correctly at the conceptual level, just not in
  the field names chosen for it.

## Options considered and trade-offs

- **Keep `from`/`to`, rely on docs to disambiguate**: no migration cost, but the confusion is a
  naming problem, not a documentation gap — better docs don't fix a name that reads wrong on
  first contact in the UI.
- **`to`→`assignee`, `from`→`writer`, `turn` unchanged**: `assignee` is the correct, industry-common
  term. `writer` was considered for `from` but rejected — `requester` already existed as this
  field's *storage* column name (ADR-0010's SQL-keyword-avoidance rename) and is used in prose
  throughout `architecture.md`/`usage.md` already; introducing a third term (`writer`) alongside
  `requester` and `from` would have added a synonym instead of removing one.
- **`to`→`assignee`, `from`→`requester`, `turn` unchanged (adopted)**: the storage column names
  (`requester`/`assignee`, chosen in ADR-0010 specifically to dodge the `FROM`/`TO` SQL-keyword
  collision) become the wire names too — one name per concept end to end, not three. `turn` itself
  was also considered for renaming (a `turn=?` open question) but kept: it never had the
  preposition-collision problem `from`/`to` did, and it isn't a SQL keyword, so nothing about it
  needed fixing — only its two possible *values* change, from `"to"`/`"from"` to
  `"assignee"`/`"requester"`, to match the fields they now name.

## Decision

Wire format (JSON field names on `Item`, `docket-mcp` tool parameters, and the `list_items`/`PATCH`
query and body keys):

```
requester: string | null   // was `from`. Who this item is being worked for. Optional, set at
                             // creation only, or via `PATCH /items/{id} {"requester": "…"}`.
assignee:  string | null   // was `to` (originally `owner`). The worker currently holding the
                             // item — set by claim, checked by submit.
turn: "requester" | "assignee" | null   // was "from" | "to" | null. Derived from `state`, not
                                          // persisted — mapping unchanged from ADR-0010:
                                          //   open     -> null        (unclaimed)
                                          //   claimed  -> "assignee"  (worker's turn to act)
                                          //   resolved -> "requester" (requester's turn to approve)
                                          //   closed   -> null        (done)
```

No change to `state`/`resolution`, the claim/submit/approve transition rules, or the two-party
model itself — this ADR renames wire-level identifiers only.

**No database migration.** The backing SQLite columns were already named `requester`/`assignee`
(ADR-0010's storage note) — only the Rust struct field names, the `#[serde(rename = "from"/"to")]`
attributes (now removed — the field names match the wire names directly), the HTTP query parameter
names (`?to=`/`?from=` → `?assignee=`/`?requester=`), and every consumer (`docket-mcp` params,
`docket-cc`'s `ItemDto`, `docket-console`'s `Item` interface and UI) change. This makes the
redeploy materially lower-risk than ADR-0010's original `owner`→`to` migration, which did require a
schema change.

**Breaking change for existing clients.** Any caller still sending `from`/`to`/`?to=`/`?from=` on
the wire, or reading `turn: "to"|"from"` from a response, breaks against a server carrying this
ADR — there is no dual-read compatibility shim. Acceptable because docket has no external consumers
yet (pre-M4, no authentication, single-owner scope per [ADR-0006](ADR-0006-single-owner-later.md));
a later ADR would need to reconsider this if that changes.

## Consequences

**Gained**: one name per concept, matching end to end from SQL storage through the HTTP/JSON wire
to the MCP tool schema to the console UI — no translation step for a reader (or an LLM agent
reading tool descriptions) to hold in their head. `assignee` reads correctly as a role in the
console's table/detail views, removing the `to`≈`topic` ambiguity that motivated this ADR.

**Given up**: nothing beyond the churn of the rename itself — no data migration, no new concept,
no change to the transition rules or the two-party model. The `topic`-as-category UI change (moving
`topic` from a table column to a grouping header) that surfaced the `to`≈`topic` problem is tracked
separately from this ADR; this ADR only fixes the field names.

## Re-open trigger

Same as [ADR-0010](ADR-0010-item-from-to-turn.md#re-open-trigger): if a workflow needs more than
two parties, `turn`'s two-value model stops being sufficient and the schema needs revisiting —
unaffected by this rename.
