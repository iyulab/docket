Status: v0 alignment snapshot | 2026-08-23 | settled

# ADR-0016: `seq` — a short numeric alias alongside the canonical UUID `id`

## Context

Every item's canonical identifier is a UUID v4 (`storage.rs`'s `Uuid::new_v4()`), and every
id-accepting endpoint/tool matches on it verbatim. Reported friction, independently confirmed
across two downstream sessions on the same day: `docket-console`'s only on-screen identifier is a
truncated 8-character slice of the UUID (`Card.tsx`'s `item.id.slice(0, 8)`), and an agent that
copies that slice — or otherwise re-types/re-truncates a UUID by hand — gets a `404`/"not found"
from `get_item`/`claim_item`/etc. with no indication that the id was merely truncated, not wrong.
Separately, an agent operating purely from conversation (no clipboard) pays the full 36-character
UUID as tokens on every reference back to an item, in every tool call.

Both symptoms trace to the same root cause: the only identifier the API exposes is long and
opaque. This is squarely inside `docket-core`'s own responsibility layer, not a consumer-domain
concept — any caller referencing items by hand or by token budget hits the same wall (`라이브러리
한계 = 개선 기회`).

## Options considered and trade-offs

**Whether to replace or supplement the UUID:**

- **Reject — replace `id` with a short identifier**: breaking change to every existing caller,
  stored tag reference (`duplicate-of:<id>`, ADR-0015), and any external record already holding a
  UUID. No evidenced need to justify a breaking migration at 0.X.X.
- **Accept — add a second, purely additive field** (adopted): `id` stays canonical and unchanged
  everywhere; the alias is a new, optional-to-use way to *reference* the same item.

**Numbering scope — global vs. per-topic:**

- **Reject — per-topic counter** (e.g. `iyulab/docket-works#12`, mirroring GitHub's per-repo issue
  numbers): the closest domain analogy (`topic` ~ repo), and the shape initially proposed when this
  friction was first triaged. Rejected on closer design: resolving a per-topic number back to an
  item requires the topic *alongside* the number — either as a compound string (`topic#12`, which
  re-embeds the same long topic path this ADR is trying to get callers away from paying for) or as
  a second parameter every id-accepting tool would need to grow. Either way it defeats the
  token-savings motivation directly, and topics here can be arbitrarily deep paths, unlike a
  bounded GitHub `org/repo`.
- **Accept — a single global monotonic counter** (adopted): one small integer resolves an item on
  its own, no topic context required — the caller-facing shape that actually delivers the
  token-savings goal this was raised for. Items are never reassigned between topics (no such
  operation exists), so a global sequence never needs renumbering.

**Storage shape:**

- **Reject — reuse SQLite's implicit `rowid`**: `items.id` is already the declared `PRIMARY KEY`
  (`TEXT`), so `rowid` is a distinct, un-exposed integer already sitting on every row — but
  `delete_item` (ADR-0013) hard-deletes rows, and SQLite reuses a freed `rowid` unless the table
  opts into `AUTOINCREMENT`, which risks a deleted item's number being handed to an unrelated new
  item. Silent alias reuse is worse than the problem this ADR fixes.
- **Accept — a dedicated `seq_counter` single-row table, advanced under the store's existing
  connection-wide mutex** (adopted): `create_item` already runs inside a transaction on the one
  mutex-guarded connection (`Store`'s own doc comment: serializing access is what makes
  `claim`/`submit`/`approve` exclusive), so `UPDATE seq_counter SET next = next + 1 ... RETURNING
  next - 1` inside that same transaction is race-free by construction, with no extra locking
  primitive. `seq` is stored as a plain column on `items`, kept in the same `SELECT` column list as
  every other field instead of requiring a `JOIN` on every read.

**Resolution — where an alias is accepted:**

- **Accept — every existing `Store` method that takes an item `id` also accepts `seq`** (adopted):
  a caller-supplied identifier that parses as an integer (with or without a leading `#`) is looked
  up in `seq_counter`'s backing column and resolved to the canonical UUID before any existing SQL
  runs; anything else is assumed to already be the canonical UUID, unchanged from today. A UUID
  never parses as a bare integer, so the two formats can't collide. Resolution lives once, inside
  `docket-core`'s `Store` — `docket-mcp` and `docket-cc` are thin HTTP/string-interpolating clients
  (verified: every `item_id`-taking MCP tool interpolates the caller's string directly into the
  request path), so neither needs any code change for this to work end-to-end.
- **Reject — resolving `duplicate_of_id` in `merge_item`**: that value is stored verbatim as an
  opaque `duplicate-of:<id>` tag (ADR-0015), with an already-documented, deliberate absence of any
  referential check that it names a real item. Resolving it here would newly reject a value that
  was previously always accepted, and would silently convert whatever the caller intended to record
  into a different string when it happened to parse as a number — a distinct concern from *finding*
  an item, out of scope for this ADR.

## Decision

```
items.seq: integer  # assigned once at INSERT time, from a single global counter; never reused,
                     # even after delete_item. Present on every Item everywhere id is (list/search/
                     # get/create response), same as any other column.

seq_counter          # one-row table: (id = 1, next). create_item does
                     #   UPDATE seq_counter SET next = next + 1 WHERE id = 1 RETURNING next - 1
                     # inside its existing transaction, under the store's single mutex-guarded
                     # connection — no new locking.

Resolution: every Store method taking an item id (get/set_item_requester/archive/delete/claim/
submit/reject/reopen/approve/remove/force_close/merge's own id (not duplicate_of_id)/add_tags/
remove_tags/add_comment/list_comments) first tries the input as `seq` (bare integer, or `#`-
prefixed) and falls back to treating it as the literal `id` — so existing callers passing full
UUIDs see no behavior change.

Legacy databases (pre-ADR-0016): `seq` backfilled in creation order (`created_at` ASC, `id` ASC
tiebreak) the first time `Store::open` runs against them, same idempotent
`pragma_table_info`-gated migration pattern as `migrate_add_archived_at`.
```

No new `state`/`resolution` value. `docket-mcp`/`docket-cc` need no code change (see above);
`docket-mcp`'s tool descriptions and `docs/usage.md` gain a note that `item_id` accepts either
form, matching the doc-only precedent set by
[Issue #26](https://github.com/iyulab/docket-works/issues/26)'s sort-order documentation.
`docket-console` gains a visible `#<seq>` next to each item (replacing `Card.tsx`'s truncated-UUID
slice, the exact display this ADR's motivating friction traced back to) and in the item detail
header.

## Consequences

**Gained**: a short, unambiguous, standalone reference for any item — no topic context needed to
resolve it, no truncation-induced 404s (the console no longer shows a prefix that looks copyable
but silently isn't a valid lookup key), and materially fewer tokens spent per reference in
conversation/tool-call contexts. Fully backward compatible — no existing caller, stored tag, or
external record holding a UUID is affected.

**Given up**: numbering is global, not per-topic — an item's `seq` says nothing about which topic
it belongs to on its own (unlike a GitHub `org/repo#N`), so a caller wanting the topic must still
read `topic` off the returned item, same as today. `merge_item`'s `duplicate_of_id` stays
UUID-only in practice for now (see rejected option above) — a caller wanting to reference a
duplicate by its short alias must resolve it to a UUID itself first; revisit if that friction is
ever actually reported.

## Re-open trigger

If items ever gain a topic-reassignment operation, a global counter's "which topic is this under"
gap (see Given up) becomes worth resolving properly rather than living with `topic` as a
separate read. If `merge_item`'s `duplicate_of_id` not accepting `seq` is reported as real friction
(not just theoretical asymmetry), revisit the rejected option above with that evidence.
