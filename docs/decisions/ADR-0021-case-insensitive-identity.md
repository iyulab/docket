Status: v0 implementation | 2026-09-08 | implemented

# ADR-0021: Identity comparison is case-insensitive

## Context

`worker id`, `requester`, `assignee` and `topic` are all *identifiers*: strings whose only job is
to say whether two references mean the same thing. Every comparison of them in `docket-core` was
byte-exact, so two spellings that differ only in case were two different identities — a different
worker, a different requester, a different topic.

That is not a hypothetical. A live audit of the running dataset (201 items, 42 topics, 23
registered workers) found **two independent drifts**, both in party-identity fields:

| Field | Spellings | Items |
|---|---|---|
| `requester` | `iyulab/Filer` (21) vs `iyulab/filer` (2) | 23 |
| `assignee` | `iyu-devstack/Schemorph` (2) vs `iyu-devstack/schemorph` (1) | 3 |

The `requester` case is the one that was reported
([docket-works#37](https://github.com/iyulab/docket-works/issues/37)); the `assignee` case was found
while measuring it, and is what makes this a class rather than an incident. Neither party ever typed
two spellings on purpose. A worker id is derived per session — sometimes computed, sometimes written
by hand — and the two paths disagreed on case.

The consequence is that an identity silently splits in half. `list_items(requester="iyulab/Filer")`
and `mine="iyulab/Filer"` returned 21 items and never the other 2; `mine="iyulab/filer"` returned
only those 2. The requester's own resume sweep therefore did not see two of its items for six days —
both already `resolved` and waiting on its approval, both with the upstream fix already released. The
approval that finally happened only worked because the caller passed the *wrong* spelling to match
what the row happened to store.

Two facts about the current state matter for what follows:

- **Zero drift in `workers` and zero in `topic`.** All 23 registered worker ids are distinct under
  case folding, all 42 topics are, and no worker's registered topics miss any item's topic only
  because of case. Topic and worker folding are included below for *consistency of the identity
  class*, not because evidence demands them.
- **Identity comparisons are mostly not in SQL.** `assignee`, `requester`, `topic_scope` and `mine`
  are filtered in Rust (`crates/docket-core/src/main.rs`), over rows SQL already returned. Only
  seven comparisons are SQL predicates. This is the fact that decides the option below.

## Options considered and trade-offs

- **`COLLATE NOCASE` on the identifier columns** — the usual SQLite answer, and it would fold every
  `=`, `IN` and uniqueness check on those columns forever, including ones not yet written. Rejected
  because it **cannot reach where most of the comparisons live**: the four filters above are Rust
  `==` on values already fetched, and `topic_matches` is Rust prefix logic. A column collation would
  fix a minority of sites while reading, in the schema, as though it had fixed all of them — the
  worst outcome available, since the next person would trust it. It also requires rebuilding the
  tables (SQLite has no `ALTER COLUMN ... COLLATE`), which is a real migration bought for partial
  coverage.
- **Normalize on write (store everything folded)** — rejected. Identities are shown to people;
  `iyulab/Filer` reading back as `iyulab/filer` everywhere is a display regression traded for a
  comparison fix, and it destroys information the API can never recover.
- **One Rust comparison primitive, plus `COLLATE NOCASE` on the seven SQL predicates (adopted)** —
  puts the rule in one function every site calls, in the language where the sites actually are. No
  schema change, so no table rebuild and no migration required for correctness.
- **Do nothing, fix the rows by hand** — rejected. It has already recurred once independently, and
  the failure mode is silence: nothing reports that an identity split, which is why the reported
  case went unseen for six days rather than surfacing as an error.

## Decision

**Two identifiers naming the same thing in different case are the same identity.** Comparison folds
case; **storage preserves what was written**.

- `docket_core::domain::identity_eq(a, b)` is the single comparison primitive. It uses
  `eq_ignore_ascii_case` — see "Given up" on the ASCII scope.
- `topic_matches` folds both its exact and its prefix arm through it.
- Every Rust-side filter routes through it: `assignee`, `requester`, `topic_scope`, and both
  identity arms of `mine`.
- Seven SQL predicates take `COLLATE NOCASE`: `submit_item`'s `assignee` match, `approve_item`'s and
  `reject_item`'s `requester` match, the `topic = ?` filter in `list_items` and in `search_items`,
  `get_worker`'s lookup, and `register_worker`'s canonical-row lookup.

**`register_worker` upserts onto the existing row and keeps its spelling.** Registering
`iyulab/filer` when `iyulab/Filer` is already registered updates that row's topics and returns
`id: "iyulab/Filer"` — one row, not two, and the caller learns the canonical spelling from the
response.

- A **`409 Conflict`** naming the existing spelling was considered and rejected. `register_worker`
  is an idempotent upsert that every session calls on startup; a conflict there would take the one
  session whose id drifted and stop it registering at all — turning a benign case difference into a
  hard failure, in exactly the scenario this ADR exists to make harmless. The response's canonical
  `id` is a better channel than an error, because the caller is a program that can adopt it, not a
  person who can go rename something.

## Consequences

**Gained**: an identity cannot silently split. The reported failure — a requester's own sweep
missing its own items — is not reachable, in either direction of spelling, for any of the four
fields. `approve_item`/`reject_item` match a drifted `requester` instead of hard-failing, which
removes the incentive that led a caller to impersonate the wrong spelling rather than correct it
(see [ADR-0019](ADR-0019-approve-reject-requester-match.md)'s 2026-09-08 note).

**Given up — ASCII only.** `eq_ignore_ascii_case` and SQLite's `NOCASE` both fold ASCII and leave
every other codepoint byte-exact. Every identity observed is ASCII, and the convention these follow
(`org/repo`) is ASCII by GitHub's own rules, so this buys the whole observed space at no allocation.
An identity with non-ASCII case variation would still split, silently, exactly as before. If one
ever appears, the fix is `to_lowercase()` in `identity_eq` and `LOWER()` in the SQL predicates —
the point of routing every site through one function is that this stays a one-place change.

**Given up — case can no longer distinguish two identities.** Two workers deliberately named
`acme/Widget` and `acme/widget` are now one worker. Nobody wants that naming, and treating it as
supported was the bug; but it is a real capability being removed, not merely a bug being fixed.

**Not changed**: tags. Tags are opaque to core ([principles.md](../principles.md) P-1) and
`related:<id>` is documented as matching the literal id string; folding them would make core
interpret values it has deliberately never interpreted. Item ids, `seq` aliases, `state`,
`resolution`, and every comparison outside the four identifier fields are untouched.

## Data migration — cosmetic only, and deliberately not automatic

Folding fixes every *query*. It does not change what the three drifted rows *store*, so a consumer
that groups by the returned string still sees two spellings. That is the only remaining symptom, and
it is cosmetic.

The canonical spelling for those rows is **the one the identity currently uses** — the most recent
write, which on this data is also the majority spelling in both cases (`iyulab/Filer`,
`iyu-devstack/Schemorph`). Note this is *not* the same rule as `register_worker`'s first-write-wins
above, and it does not need to be: the upsert answers "which existing row does this registration
land on", where exactly one row exists; the migration answers "which of two historical strings should
be displayed", where the current one is the useful answer. After this ADR, no future drift needs
migrating at all — folding makes it invisible — so this rule governs one cleanup, not an invariant.

Execution is **not** performed here. It mutates the running dataset, and the two halves are not even
symmetric: `requester` rows can be corrected through `set_item_requester`, which since
docket-works#37 records a before→after comment and therefore documents itself; the single `assignee`
row has no edit path at all (`set_item_assignee` does not exist, and inventing one to fix one row
would be a worse trade than a one-line `UPDATE`). So it is a human-run step, parked with the release
gate rather than executed by the change that made it optional.

## Re-open trigger

A non-ASCII identity appearing in any of the four fields (see "Given up"), or evidence that a
deliberate case distinction between two identities is actually wanted.

## Related

[ADR-0022](ADR-0022-identity-alias.md) generalizes this same seam: case folding is a *fixed* rule
for when two spellings are the same identity, and ADR-0022 adds a *declared* one, on the same
comparison primitive and the same four-field identity class.
