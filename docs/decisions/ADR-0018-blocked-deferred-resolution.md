Status: v0 implementation | 2026-08-23 | implemented

# ADR-0018: `blocked`/`deferred` resolutions — a worker-facing, MCP-exposed way to park an item

## Context

Consumers already needed a way to say "this item is genuinely open, but nothing can move it
forward right now" — a concrete external dependency (no access to a paywalled standard), or
unproven cross-consumer demand (a feature only one consumer has asked for, per the
upstream-extension policy's demand-driven-growth principle). With no core primitive for this, the
convention that emerged organically was `state = open` plus a free-form tag (`blocked`/`deferred`)
— `tag`/`comment` were exactly designed for this kind of workflow-generic labeling
([ADR-0009](ADR-0009-tag-and-comment-vocabulary.md)).

In practice this convention doesn't hold up as a *filtering* primitive. A live audit of a real
deployment (2026-08-23) found 15 items across 6 topics carrying `open` + `blocked`/`deferred`,
none of them distinguishable from a genuinely actionable open item by any `list_items`/
`search_items` call — neither tool accepts a tag filter capable of *excluding* a tag (`search_items`
only supports "any/all of these tags", never "none of these"), so every "what's actually open"
query a consumer runs is unavoidably polluted by items nobody can act on. `list_tags`/`search_items`
were never meant to carry structural query guarantees for tag content — ADR-0009 is explicit that
tags are opaque, caller-defined strings the core never interprets — so this isn't a bug in that
design, it's a sign the concept being represented isn't actually a tag.

The first draft of a fix considered here promoted "blocked" to a new first-class axis alongside
`state`/`turn` (a `held_at`/`label` field pair, mirroring `archived_at`). That draft was dropped
before implementation: it required either widening `turn` to a third value (breaking
[ADR-0010](ADR-0010-item-from-to-turn.md)'s "turn is a pure function of state, two values plus
closed's `null`" invariant) or inventing a second derived-field computation path alongside it, for
a distinction — "is anyone's turn actionable" — that `state = closed` (turn = `null`) already
expresses for free. A second turn discussion, prompted directly by this question, also surfaced
that "blocked, read this and decide" (a genuine handoff to the other party) and "blocked, dormant,
nobody's job until an external condition changes" are different things — but the only real
instance of the former on record (the `shell-tunnel` throughput item) was carried entirely through
`add_comment`, with `turn` never touched, and worked fine. There is no recorded case needing a
turn-flipping "notify" mechanism; building one now would be speculative (YAGNI).

## Options considered and trade-offs

**Where the "parked" concept lives:**

- **Reject — keep it as a tag, add tag-exclude filtering to `list_items`/`search_items`**: the
  minimal fix originally proposed, now superseded by this ADR. Rejected because it doesn't address
  the actual objection: a tag is an arbitrary,
  unconstrained string (typo-prone, no casing guarantee, no compile-time exhaustiveness) that
  filtering/rendering logic would still depend on by convention only, which is exactly the kind of
  reliance ADR-0009 designed tags to be unsuited for.
- **Reject — a new first-class `held`/`label` field pair alongside `state`/`turn`**: see Context —
  requires either a third `turn` value or a second derivation path duplicating `state`'s existing
  "is this actionable" signal, for no benefit `state = closed` doesn't already give for free once
  paired with a `resolution` value.
- **Accept — two new `Resolution` values, `blocked`/`deferred`, closing the item** (adopted): reuses
  100% of existing machinery. `state = closed` already gives `turn = null` ("nobody's turn")
  ([ADR-0010](ADR-0010-item-from-to-turn.md)) and is already excluded by any `list_items`/
  `search_items` call scoped to `open`/`claimed` (exactly what `docket`-consuming session resume
  flows already query) — zero new fields, zero new query parameters. `reopen_item`
  ([ADR-0012](ADR-0012-item-reject-reopen-transitions.md)) already does exactly "bring a closed item
  back, clearing `resolution`, with a required reason" — the unblocking half of this feature already
  existed before this ADR. `docket-console`'s `Card.tsx` already renders `resolution` as a badge —
  two new label/color entries are the entire UI change.

**Admin close (`close_with_resolution`, HTTP-only) vs. a new worker-facing pair:**

- **Reject — reuse `force_close_item`'s shape (state-unrestricted admin close, `resolution =`
  whatever, HTTP/console-only)**: this is the shape a first instinct reaches for, since it's the
  closest existing precedent for "close from any state with an arbitrary resolution." Rejected
  because `force-close`/`force-approve`/`remove`/`merge` are HTTP-only by the
  [MCP-exposure rule](../architecture.md#mcp-exposure-rule) specifically because they are admin
  overrides of the two-party `claim → submit → approve` handshake — marking something blocked or
  deferred is not that. It is the same kind of thing `reject_item` already is: a normal worker
  judgment call, made constantly by whichever session is doing the actual triage work, that needs
  to be callable from `docket-mcp` — the tool surface agents actually operate through. Making it
  HTTP-only would mean an agent doing routine triage (the majority of real-world usage) could
  observe `resolution = blocked` on `get_item` but could never *set* it, defeating the
  original ask ("console **and mcp** should read the current situation correctly").
- **Accept — `block_item`/`defer_item`, MCP-exposed, requiring a reason** (adopted): same
  state-unrestricted SQL shape as the admin closes (any pre-closed state, no assignee check) via a
  new shared `close_with_reason` helper, but the lifecycle comment carries the caller's free-text
  `reason` verbatim — matching `reject_item`/`reopen_item`, not the bare op-name marker
  `close_with_resolution` writes. `reason` is required (blank rejected) because it's the only record
  of *why*, and that's load-bearing for whoever later runs `reopen_item`.

**One parameterized op vs. two named ops:**

- **Reject — a single `defer_item(item_id, resolution, reason)` taking either value**: would need
  either a new restricted enum (to stop a caller passing `done`/`duplicate`/etc.) or an unchecked
  string, both worse than the type system already doing this for free.
- **Accept — `block_item`/`defer_item` as two thin wrappers over one private `close_with_reason`
  helper** (adopted): matches the existing pattern exactly (`remove_item`/`merge_item`/
  `force_close_item`/`force_approve_item` are four thin wrappers over `close_with_resolution`). Two
  named tools also give `docket-console`/an MCP client's tool list two self-documenting verbs
  instead of one with a mode argument.

## Decision

```
(any pre-closed state) --block(author, reason)--> closed(blocked)    # new MCP tool
(any pre-closed state) --defer(author, reason)--> closed(deferred)   # new MCP tool
closed(blocked|deferred) --reopen(author, reason)--> open|claimed    # existing tool, unchanged

Store::close_with_reason(id, resolution, op, author, reason)
  # sibling to close_with_resolution: same any-pre-closed-state UPDATE, but records `reason`
  # verbatim as the lifecycle comment instead of a bare op-name marker.
Store::block_item(id, author, reason)  = close_with_reason(id, Resolution::Blocked,  "block", author, reason)
Store::defer_item(id, author, reason)  = close_with_reason(id, Resolution::Deferred, "defer", author, reason)

POST /items/{id}/block {"author"?, "reason"}   # ReasonedRequest, same shape as reject/reopen
POST /items/{id}/defer {"author"?, "reason"}

docket-mcp: block_item / defer_item tools, Parameters<ReasonedParams> (item_id, author?, reason)
  — the same params struct reject_item/reopen_item already use, no new type.
```

`Resolution` gains two variants (`Blocked`, `Deferred`) — a Rust enum with exhaustive `match`, not
an opaque string, so every consumer that pattern-matches on it (including `docket-console`'s
`RESOLUTION_LABEL: Record<Resolution, string>`) fails to compile until updated, rather than
silently mis-rendering an unrecognized value. `docket-cc`/`docket-mcp`'s `ItemDto.resolution` stays
`Option<String>` (unchanged) — both already treat `resolution` as an opaque passthrough field, so
neither needed a code change.

## Consequences

**Gained**: `list_items(state="open")`/`search_items(...)` — including the exact query
`docket`-consuming session-resume flows already run — stop surfacing parked items as if they were
actionable, with no new query parameter to learn. A worker doing routine triage (the common case)
gets a first-class, reason-required, fully reversible way to say "parked" that shows up correctly
in `docket-console` for free. The distinction is now type-checked (`Resolution` is a closed enum)
rather than convention-dependent (a tag string).

**Given up**: `state = closed` no longer means "someone made a final disposition judgment
(done/duplicate/wontfix/invalid) or an admin overrode the handshake" — it now also covers "a worker
parked this and expects it back." A reader that needs "genuinely finished, in any sense" excluding
parked items must filter `resolution` to the original four values, not just check `state`. The
existing `blocked`/`deferred` *tags* on already-migrated items are not retroactively removed by this
ADR — cleaning up the 15-item backlog (re-triaging each into `block_item`/`defer_item` with a reason
and dropping the now-redundant tag) is a follow-up migration pass, not part of this decision.

## Re-open trigger

If a real case shows up needing the "hand this to the other party to read and decide" flavor of
blocking discussed in Context (as opposed to "dormant until an external condition changes") — i.e.
a `turn`-flipping notify step actually gets requested rather than handled adequately through
`add_comment` — revisit `block_item`/`defer_item` to add that as an explicit option, gated on that
real usage evidence rather than building it speculatively now.
