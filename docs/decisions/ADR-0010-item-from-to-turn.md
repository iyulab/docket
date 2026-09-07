Status: v0 alignment snapshot | 2026-08-18 | settled — field names superseded by [ADR-0011](ADR-0011-requester-assignee-naming.md)

# ADR-0010: Item `from`/`to`/`turn` — explicit two-party handoff fields

> **Naming update ([ADR-0011](ADR-0011-requester-assignee-naming.md), same day)**: the wire field
> names below (`from`, `to`) were renamed to `requester`/`assignee` — `topic` and `to` had converged
> to mean nearly the same thing in the console, and `from`/`to` read as directional prepositions
> rather than roles. `turn`'s values changed to match (`"assignee"`/`"requester"` instead of
> `"to"`/`"from"`); the `turn` field name itself, and every other decision in this ADR (the
> two-party model, the derived-not-stored `turn`, the `requester`/`assignee` *storage* column names
> that motivated the original wire names' SQL-keyword avoidance), are unaffected. Everything below
> is read with that substitution in mind — kept as originally written rather than edited, per this
> project's "corrected, not silently edited" convention.

## Context

`Item.owner` is used for exactly one thing: the worker id currently holding the item (set by
`claim`, checked by `submit`). It correctly answers "who is working this," but nothing in the
schema answers the complementary question — "who is this being worked *for*." That side of the
relationship has so far existed only as an informal tag convention (a caller adding a
`found-in:<origin>`-shaped tag by hand), which is invisible to any consumer that doesn't already
know to look for it, and isn't queryable as a first-class value.

Once an item is understood as a two-party handoff — a requester waiting on a worker, then the
worker handing back to the requester for approval — the schema should say so directly instead of
making every consumer re-derive it from `state` plus a tag-string convention.

## Options considered and trade-offs

- **Keep `owner`, document the informal tag convention better**: no schema change, but "who filed
  this" stays unqueryable, and consumer-specific parsing of a free-form tag is fragile.
- **Replace `state`/`owner` with a new from/to/turn-only lifecycle**: more uniform, but breaking —
  every existing consumer of `claim`/`submit`/`approve` and the ownership list filter would need to
  change, and the four-state lifecycle ([ADR-0003](ADR-0003-item-state-schema.md)) already earned
  its simplicity through review; nothing here invalidates it.
- **Rename `owner`→`to`, add `from`, derive `turn` (adopted)**: keeps the settled state machine and
  its transition rules untouched — `to` is exactly what `owner` already meant, just named for its
  role in the relationship instead of the mechanic that sets it. `from` is new, optional, explicit.
  `turn` is *derived*, never persisted, so it can never drift out of sync with `state` — the same
  reasoning ADR-0003 used to leave `expired` out of the stored schema rather than let a derivable
  fact double as a second source of truth.

## Decision

Wire format (JSON field names on `Item`, and `docket-mcp` tool parameters):

```
from: string | null   // who this item is being worked for. Optional, set at creation only —
                        // never auto-derived from tags; a caller identifies itself explicitly or
                        // not at all.
to:   string | null   // was `owner` — the worker currently holding the item (claim/submit-checked)
turn: "from" | "to" | null   // derived from `state`, not persisted:
                              //   open     -> null   (unclaimed, no current holder)
                              //   claimed  -> "to"    (worker's turn to act)
                              //   resolved -> "from"  (requester's turn to approve)
                              //   closed   -> null   (done)
```

No change to `state`/`resolution` or to the claim/submit/approve transition rules — `to` is a
rename of what `owner` already tracked, not a new mechanic.

**Storage note**: the backing SQLite columns are named `requester`/`assignee`, not `from`/`to` —
both are SQL keywords (`FROM` appears in essentially every statement; `TO` in `RENAME TO`, foreign
key clauses), and quoting every occurrence throughout the query strings was judged a worse trade
than a one-line rename at the Rust/JSON boundary (`#[serde(rename = "from"/"to")]`). Every consumer
of the HTTP API and the MCP tools sees `from`/`to` either way — this is purely an internal
implementation detail of `docket-core`'s SQL layer.

**List filter**: the existing `owned_by` filter conflated two different questions under one name —
"items this worker currently holds" (never actually implemented that way) and "items under a topic
this worker is registered for" (what it actually did — a topic-jurisdiction filter, unrelated to
who holds any given item). This change splits it into two honestly-named filters:
`to=<worker_id>` (exact match against the new field) and `topic_scope=<worker_id>` (the
pre-existing topic-prefix behavior, renamed).

## Consequences

**Gained**: "who is this for" becomes a first-class, queryable field instead of tag archaeology.
Every consumer (console, `docket-mcp` tools) can show whose turn it is without re-deriving it from
`state`. The `to`/`topic_scope` split fixes a filter that silently returned the wrong items for its
documented purpose.

**Given up**: `from` is opt-in at creation — existing items and callers that don't pass it simply
have `from = null`; there is no attempt to backfill it from the pre-existing tag convention, which
remains a separate, unrelated mechanism.

**Naming note**: the `found-in:<topic>` tag convention (see [glossary.md](../glossary.md)) already
uses "from"/"to" in prose to describe a **topic-to-topic** reference (which topic found an item vs.
which topic owns it). This ADR's `from`/`to` are a different axis — a **party-to-party** relationship
(which requester vs. which worker) — and the two can coexist on the same item without being related.
Kept the same vocabulary anyway because it's the accurate English word for both relationships and
disambiguating with alternate names (e.g. `requester_topic` vs `requester_id`) would obscure the
parallel rather than clarify it; glossary.md documents the distinction explicitly.

## Re-open trigger

If a workflow needs more than two parties (e.g., a reviewer distinct from both `from` and `to`),
`turn`'s two-value model stops being sufficient and the schema needs revisiting.

## 2026-08-18 update — `found-in:` superseded, not a parallel axis

The "Naming note" and "Given up" sections above have not held up in practice and are corrected
here rather than silently edited, so the original reasoning stays visible:

- **`found-in:` is deprecated, not a separate axis.** In actual use, `from` is populated with
  exactly the value the `found-in:<topic>` tag used to carry (the discoverer). Treating them as
  unrelated per-item concepts was wrong — `from` is the proper field for what the tag was standing
  in for. New items should set `from` at creation instead of adding the tag; see
  [glossary.md](../glossary.md)'s updated `found-in:` entry.
- **The backfill did happen.** `PATCH /items/{id} {"from": "…"}` was added (admin-only, HTTP-only,
  no MCP tool — same reasoning as the three close operations) specifically to set `from` on items
  that predate this ADR. Every non-closed production item carrying a `found-in:` tag was backfilled
  from that tag's value, then the now-redundant tag removed via the existing `remove_tags` — see
  `docket-works` HISTORY for the 2026-08-18 entries. Closed items were left untouched (no
  practical need surfaced yet, and their `found-in:` tag remains the only record of the value until
  one exists).
- **`to`'s display fallback lives in the console, not here.** `docket-console` shows `to`, falling
  back to the item's own `topic` when nobody has claimed it yet, so "who should look at this" always
  has an answer. This is display-only — `Item.to` itself is unchanged, still `null` until `claim`.

## 2026-08-18 update — `turn` also signals `open`, not just `claimed`/`resolved`

The `open -> null` row in `turn`'s mapping above has not held up in practice and is corrected here
rather than silently edited:

- **`open` is the assignee's turn, not nobody's.** An open item is unclaimed, but it's squarely
  waiting on whichever topic owns it to look at it and act — exactly the same as `claimed`, just
  before a specific worker has picked it up. Mapping `open` to `null` conflated "actionable, waiting
  on someone" with `closed`'s "done, waiting on no one" — precisely the distinction `turn` exists to
  make, and the one case the "to's display fallback" note above (`assignee ?? topic`, so the console
  always has *some* answer for "who should look at this") had already worked around for the assignee
  column but not for `turn` itself.
- **`Item::turn_for` now maps `open -> Some(Turn::Assignee)`** (`claimed`/`resolved`/`closed`
  unchanged — only `closed` is still `null`). `assignee` the field is still `null` until `claim`;
  only the derived `turn` value changed. A caller that wants "is this item unclaimed" still reads
  `state`, same as before — `turn == null` now means exactly `closed`, not "closed or open".
- No wire/schema change — same three `Turn` values, same field. Every existing `turn` consumer
  (console, `docket-mcp`) picks this up automatically since they already render/branch on
  `assignee`/`requester`/`null` without assuming which state produces which.

## 2026-09-08 update — `resolved` means "the requester's turn", not "done"

The `resolved -> "from"` row in `turn`'s mapping above is unchanged, but the *meaning* the rest of
this project attached to it has not held up, and is corrected here rather than silently edited.

**What went wrong.** `turn` was defined as derived from `state`, and `resolved` was described
everywhere else — the `submit_item` tool, `docs/usage.md`, the `mine` filter — as "submitted as
done, awaiting approval". Those two readings are not the same claim, and an assignee eventually hit
the gap: it had done a first pass and needed two decisions from the requester before it could go
further. From that moment the item was waiting on the requester, but the only door into
`turn = requester` was labelled *completion*, so the assignee judged that submitting would assert
something false and left the item in `claimed`.

That judgment made things strictly worse, because `claimed` is the one state the requester cannot
see. `mine` matches items where the caller is the `assignee`, `resolved` items the caller filed, and
unclaimed `open` items in the caller's topics — an item the *assignee* is holding appears in none of
them. The questions sat in the comment thread, which nothing surfaces on its own. Recovery took a
human noticing.

So the accurate label and the visibility of the handoff had been put in tension, and following the
documentation as written chose the side that loses the handoff — silently, with no query anywhere
reporting that the item was waiting on anyone.

**Decision.** `resolved` means: *the assignee cannot take this further; the requester decides what
happens next.* Finished work is the common case, not the definition. Waiting on an answer only the
requester can give is the same fact about whose turn it is, and belongs in the same state.

Consequently:

- **`submit_item` is a turn handoff, not a completion report.** Its `reason` (added with this
  update, optional) is what says which one it is. An assignee blocked on a requester decision
  submits, with the question as the reason.
- **`reject_item` carries the answer back.** Its required `reason` already had the right shape; only
  its framing was too narrow. Answering a question is as ordinary a use as sending back rework.
- **No new state.** `awaiting-requester` was considered and rejected — see below.

**Options considered.**

- **A third state (`awaiting-requester`, `turn = requester`, still "open")** — the label would be
  exactly right, and rejected for that being all it buys. `state` is read by `turn_for`, by `mine`'s
  three-way OR, by `list_items(state=)`, by `resolution`'s "only meaningful while closed" rule, and
  by the console; a fourth pre-closed state has to be threaded through every one of them, and every
  future consumer inherits the wider vocabulary. `principles.md` ranks simplicity first, and the
  handoff this state would express is already expressible.
- **Redefine `resolved`, document the round trip (adopted)** — no schema change, no new vocabulary,
  and the two transitions that carry the round trip already exist and already require the reasons
  that carry it. What was missing was never a mechanism; it was a sentence saying this is what the
  mechanism is for.
- **Leave it, and surface `claimed` questions some other way** — rejected. Any such mechanism (a
  tag convention, a comment-scanning heuristic, a fourth `mine` clause) reintroduces the
  tag-archaeology this ADR replaced in the first place, and leaves `turn` — the field whose entire
  job is answering "whose hand is this in" — still wrong about this item.

**Given up.** A question-and-answer round trip now shows in the history as a `reject`. That reads as
a verdict when it was an answer, and no amount of documentation makes the word stop looking like
one. Accepted knowingly: the alternative was a fourth state bought purely for the label, and
`submit_item`'s new `reason` puts the actual intent in the thread at the transition itself, where a
reader meets it before the word "rejected".

**Not changed.** `turn`'s derivation, its three values, the four states, the transition rules, and
every wire field. This update is about what `resolved` *asserts*; nothing about how it is reached or
represented moves.

**Implemented** (2026-09-08, same day as this decision): `submit_item` gained an optional `reason`
recorded as an atomic lifecycle comment (`storage.rs`, `POST /items/{id}/submit`, and the MCP tool
— the same `insert_lifecycle_comment` pattern `reject`/`reopen`/`approve`/`close_with_resolution`
already share). Tool descriptions for `submit_item`/`reject_item` and the `mine` filter, plus
`docs/usage.md`'s `submit_item`/`reject_item` rows, `turn` derivation note, `mine` description and
round-trip narrative, were rewritten to the definition above. `docket-console` needed no change: it
renders raw `state` values and never carried an "awaiting approval" label of its own.

## Re-open trigger (2026-09-08)

If the `reject`-reads-as-verdict cost shows up as a real misreading — someone treating an answered
question as rejected work in a way that changes what they do — the cheap next step is a distinct
`resolution`-style label on the transition, not a new `state`. Revisit then, with that evidence.
