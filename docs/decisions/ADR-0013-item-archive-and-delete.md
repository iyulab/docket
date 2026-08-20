Status: v0 alignment snapshot | 2026-08-20 | settled

# ADR-0013: Item `archive`/`delete`, and an MCP-exposure rule

## Context

Two gaps surfaced while working through [ADR-0012](ADR-0012-item-reject-reopen-transitions.md):

1. **No delete, at any layer.** An item created by mistake, or a smoke/throwaway item
   used to reproduce a defect, has no way to be removed — only `remove_item` exists,
   and it *closes* the item with `resolution=invalid`, which keeps a permanent row.
   On a live instance this has already produced a handful of items nobody can get rid
   of, including one whose own title says it is safe to remove.
2. **List/search cost grows with total history, not with what's relevant.**
   `list_items`/`search_items` return every matching row regardless of age or
   relevance, `body` included. As the number of finished items accumulates, a caller
   pays that cost on every broad query even though old, finished work is rarely what
   a query is actually looking for.

Deciding these together because both are about an item's life *after* it reaches a
terminal-ish point, and because designing one surfaced a rule the project has already
been following inconsistently: `remove`/`merge`/`force-close` exist as HTTP endpoints
but were never exposed as MCP tools — an unwritten precedent that a new destructive
or admin-flavored operation should follow, or explicitly break from.

## Options considered and trade-offs

**Delete — scope:**

- **Reject — restrict to already-`closed` items**: structurally prevents deleting
  work in progress, but forces a two-step "force-close, then delete" for the exact
  case this exists to serve (an item that should never have existed) and
  [ADR-0012](ADR-0012-item-reject-reopen-transitions.md) already established that
  the four existing closing operations are state-unrestricted (any pre-closed state)
  — restricting only `delete` would be the one inconsistent operation in the set.
- **Accept — unrestricted, any state** (adopted): matches `remove`/`merge`/
  `force-close`'s existing precedent exactly. The caller is trusted with the same
  judgment call those three already require.

**Delete — what it does to history:**

- **Reject — soft-delete (a flag, row kept)**: this is what `archive` already is once
  archive exists; a second soft-delete mechanism next to it would be a redundant
  third state axis for the same underlying fact (row hidden from view but present).
- **Accept — hard `DELETE`, cascading to that item's tags and comments** (adopted):
  a real, irreversible removal, distinct in kind (not just degree) from `remove_item`
  — `remove_item` keeps a permanent, traceable "this was invalid" record;
  `delete_item` is for the case where no record should remain at all. No `author`/
  `reason` parameter: there is no item left afterward to attach either to, and a
  caller wanting a trace of *why* something was removed should reach for
  `remove_item` instead. A dangling free-form tag on some other item that happened
  to reference the deleted item's id by convention (e.g. a `related:<id>`-shaped
  tag) is an accepted, known consequence — the core cannot know a tag's string
  encodes a reference, per `principles.md` P-1 (tags are opaque), so it cannot keep
  such a reference consistent either.

**Delete — MCP exposure:**

- **Reject — expose as an MCP tool**: consistent with every other new operation in
  this design pass, but breaks with the unwritten `remove`/`merge`/`force-close`
  precedent, and does so for the single most irreversible operation in the entire
  API.
- **Accept — HTTP-only (`DELETE /items/{id}`), no MCP tool** (adopted): this is the
  precedent made explicit rather than broken. See "MCP-exposure rule" below.

**Archive — representation:**

- **Reject — a `state`/`resolution` value**: archival is orthogonal to workflow
  position — a `closed` item is the common case but an old, abandoned `open` item is
  just as plausible a candidate, and [ADR-0012](ADR-0012-item-reject-reopen-transitions.md)
  already established that `state` means "current workflow position" and nothing
  else. A value that can coexist with any `state` does not belong inside `state`.
- **Accept — a nullable `archived_at` timestamp column** (adopted): unlike `turn`/
  `open` ([ADR-0010](ADR-0010-item-from-to-turn.md), [ADR-0012](ADR-0012-item-reject-reopen-transitions.md)),
  this fact is not derivable from anything else already stored — it is an
  independent, caller-set fact, so a computed field is not an option here, and a
  stored column is the correct (not merely convenient) choice. A timestamp rather
  than a bare boolean, matching every other lifecycle fact this schema already
  timestamps (`created_at`/`updated_at`), and enabling age-based queries later
  (e.g. "archived over N days ago") without a second column.

**Archive — reversibility and idempotency:**

- **Accept — no `unarchive` operation.** Nothing in the schema prevents adding one
  later (setting `archived_at` back to `NULL` is not destructive), so this is a
  surface-area decision, not a one-way door. Left out for now on YAGNI grounds — no
  observed need yet.
- **Accept — `archive_item` is idempotent.** Archiving an already-archived item
  returns it unchanged rather than erroring, matching `add_tags`/`remove_tags`'s
  existing idempotency convention.

**Archive — MCP exposure:**

- **Accept — expose as an MCP tool.** Unlike `delete`, nothing is lost — the data
  stays fully intact and fully reachable via an explicit query. Routine, low-stakes
  hygiene (a worker archiving its own topic's old finished items to keep queries
  fast) is exactly the kind of thing a worker should be able to do without a human
  in the loop, which is the dividing line the MCP-exposure rule below names directly.

## Decision

```
items.archived_at: INTEGER NULL   # new column, migrated in the same
                                   # pragma_table_info-guarded style as
                                   # ADR-0010's owner->assignee migration

archive_item(item_id) -> Item
  # UPDATE items SET archived_at = ?1, updated_at = ?1
  #   WHERE id = ?2 AND archived_at IS NULL
  # affected == 0 is not an error here (idempotent) — re-read and return as-is
  # no state restriction, no author/reason

delete_item(item_id) -> ()        # DELETE /items/{id}, HTTP-only, no MCP tool
  # DELETE FROM item_tags WHERE item_id = ?1
  # DELETE FROM item_comments WHERE item_id = ?1
  # DELETE FROM items WHERE id = ?1
  # no state restriction, no author/reason
```

`list_items`/`search_items` gain one new parameter, `archived: bool` (default
`false`): `false` filters to `archived_at IS NULL` (today's behavior, unaffected by
default); `true` filters to `archived_at IS NOT NULL` — an explicit archive-only
browse, matching the two views this ADR sets out to support. `get_item` and
`list_comments` are unaffected — both already take a specific id, and archival only
changes what a *broad* listing surfaces.

`comments_fts` currently has an insert trigger but no delete trigger (dead code path
until now, since nothing ever deleted a comment row) — `delete_item`'s cascade needs
a `comments_fts_ad` trigger added alongside it, mirroring `items_fts_ad`, or deleted
comments leave stale entries in that index.

### MCP-exposure rule (new, generalizing an existing unwritten precedent)

An operation gets an MCP tool when it is safe for a worker to call on its own
judgment — reversible, or destructive only to something disposable (a claim, a tag).
An operation stays HTTP/console-only when it is irreversible against durable history,
or represents an admin/human value judgment about an item's disposition
(`remove`/`merge`/`force-close`, now joined by `delete`). This was already the
project's practice; it was never written down before this ADR.

## Consequences

**Gained**: a real way to remove mistaken or throwaway items, and a way to keep
`list_items`/`search_items`'s default cost bounded by *live* history rather than
*all* history, without touching the workflow-position schema
[ADR-0012](ADR-0012-item-reject-reopen-transitions.md) just settled. The
MCP-exposure line is now a named, citable rule instead of something a future
contributor has to reverse-engineer from which four endpoints happen to lack a
`#[tool(...)]` attribute.

**Given up**: a deleted item's dangling references in other items' free-form tags
(the `related:`/`superseded-by:`-shaped convention observed in live data) are not
cleaned up and are not detectable by the core. `archived: bool` only supports two
views (default-excluded / archive-only); "everything, archived and active together"
is not available without a code change if that turns out to be needed.

## Re-open trigger

If "everything including archived" becomes a real, frequent query need, extend
`archived` from `bool` to a three-state filter rather than adding a second parameter.
If an `unarchive` need is observed, it is a small additive change (clear
`archived_at`), not a re-open of this ADR's reasoning.
