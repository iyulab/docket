Status: v0 alignment snapshot | 2026-08-20 | settled

# ADR-0015: `merge_item` records what it duplicates

## Context

`merge_item` closes an item with `resolution = duplicate`, but records nothing about *which* item
it duplicates — `resolution = duplicate` alone answers "this is a duplicate" and nothing else.
Every other admin close names a reason a reader can act on directly (`invalid`, `wontfix`); `merge`
is the one exception, and has been since [ADR-0012](ADR-0012-item-reject-reopen-transitions.md)
first named this gap and deliberately deferred it as out of that ADR's scope.

## Options considered and trade-offs

- **Reject — a dedicated `duplicate_of: Option<String>` column**: matches how `archived_at`
  ([ADR-0013](ADR-0013-item-archive-and-delete.md)) was reasoned about — a genuinely independent,
  caller-set fact, not derivable from anything else stored, so a computed field isn't an option.
  Structurally defensible on that basis alone, but it grows the schema for a single-purpose
  reference the project already has a lighter-weight, existing convention for (below), and adds a
  new column, a migration, and API/schema surface across HTTP for one narrow admin-op field.
- **Accept — a free-form tag, `duplicate-of:<id>`** (adopted): reuses the reference-by-tag
  convention this project already leans on informally (a `related:<id>`-shaped tag is named as
  precedent in `delete_item`'s own doc comment) rather than introducing a second reference
  mechanism next to it. Tags are already opaque, caller-defined strings the store doesn't interpret
  specially (`principles.md` P-1) — this fits that shape exactly: the store writes the string, it
  doesn't validate or resolve it. No schema change, no migration.
- **Reject — leave it to the caller to `add_tags` separately, after `merge_item`**: preserves the
  narrow single-purpose signature, but splits one atomic-in-intent action into two calls a caller
  can forget the second half of — which is exactly how this gap has stayed open since ADR-0012
  named it. The tag needs to be part of the same operation, not a follow-up convention no one is
  forced to observe.
- **Accept — `merge_item` takes a required `duplicate_of_id` and writes the tag atomically**
  (adopted): one call, same transaction as the `resolution = duplicate` state change. `duplicate_of_id`
  is required (rejected blank, same validation shape as `reject_item`/`reopen_item`'s `reason`) —
  an optional field would leave the exact gap this ADR exists to close: a caller could still omit
  it and land back at an untraceable `duplicate`. This is a breaking change to `merge_item`'s
  signature (HTTP body, `Store::merge_item`, `docket-console`'s `Merge` button all need the new
  field) — acceptable pre-1.0, and the alternative (an optional field) doesn't actually solve the
  problem.
- **Accept — no referential check that `duplicate_of_id` names a real item** (adopted): tags stay
  opaque to the store (P-1); validating a tag's *content* would be the store starting to interpret
  tag strings specially, which is the exact line P-1 draws. Same accepted-consequence shape as a
  dangling `related:<id>` tag surviving `delete_item` ([ADR-0013](ADR-0013-item-archive-and-delete.md)).
- **Accept — HTTP/console-only, not an MCP tool**: unchanged from `merge_item`'s existing
  MCP-exposure status (an admin/human value judgment about an item's disposition, per ADR-0013's
  MCP-exposure rule) — this ADR only changes what `merge` records, not who may call it.

## Decision

```
POST /items/{id}/merge {"duplicate_of_id", "author"?}   # duplicate_of_id required, non-blank
  # UPDATE items SET state='closed', resolution='duplicate', updated_at=?
  #   WHERE id=? AND state != 'closed'
  # INSERT INTO item_comments (...)          # author, atomic, same as the other three closes
  # INSERT OR IGNORE INTO item_tags (item_id, tag) VALUES (id, 'duplicate-of:' || duplicate_of_id)
  # all three statements in one transaction — a caller sees either the full state
  #   change + tag, or nothing (Store::merge_item no longer delegates to the shared
  #   close_with_resolution helper remove_item/force_close_item still use, since only
  #   merge needs the extra required field and tag write)
```

No new `state`/`resolution` value, no schema/table change.

## Consequences

**Gained**: `resolution = duplicate` is traceable to the item it duplicates, closing the one
exception among the four admin closes that carried no actionable reason. The reference lives next
to every other free-form reference this project already writes as tags, rather than introducing a
second, schema-backed reference mechanism alongside it.

**Given up**: `duplicate_of_id` isn't validated against real items — a typo'd or since-deleted id
tags the merged item with a reference that resolves to nothing, discoverable only by a caller
actually looking the referenced id up. Not queryable via a dedicated index/filter the way a column
would be (`list_items`/`search_items` have no `duplicate_of=` parameter) — finding "everything that
duplicates X" means a tag-prefix scan, not a column lookup.

## Re-open trigger

If "find everything that duplicates item X" becomes a frequent, real query need that a tag-prefix
scan genuinely can't serve well, revisit the rejected `duplicate_of` column option above with that
usage evidence.
