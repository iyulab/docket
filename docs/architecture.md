Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Architecture

Covers only Type-1 (hard-to-reverse) decisions. Everything else is deliberately left open until implementation forces a choice.

## Four layers

| # | Name | Responsibility | Knows its consumer? |
|---|---|---|---|
| 1 | `docket-core` | Headless work queue. Worker/topic/item/claim/stall/budget. HTTP API, poll-only | No |
| 2 | `docket-mcp` | Exposes the core over MCP. Active (pull) surface | AI in general |
| 3 | `docket-cc` | Claude Code adapter. Local daemon, hook endpoints, file representation, identifier mapping. Passive (push) surface | Claude Code |
| 4 | `docket-console` | Admin UI. A pure client of the core API | Human |

Dependencies flow one way: `console → core`, `mcp → core`, `cc → mcp → core`. Never the reverse.

**Why the boundary is drawn here**: layers 1 and 4 don't need to know whether their consumer is human or AI — they're things that would exist even without AI. Only 2 and 3 are AI-only. The line between 2 and 3 lands exactly on the pull/push boundary — an MCP tool is purely pull (the model has to decide to call it), while a hook pushed in at a turn boundary is push. Full reasoning: [ADR-0002](decisions/ADR-0002-four-layer-architecture.md).

**Implementation language**: `docket-core` · `docket-cc` · `docket-mcp` = Rust (settled), `docket-console` alone is web. Unifying the three layers on one language doesn't conflict with P-1 (core doesn't know its consumers) — layers still communicate over HTTP, so the boundary is drawn by protocol, not language. Rationale: [ADR-0007](decisions/ADR-0007-language-runtime.md).

## Single repo, enforced by mechanism

Since each layer has exactly one consumer, we don't split it across repos. Instead, three mechanisms prevent leakage between layers:

1. **Dependency-direction checking** — the build fails if a core package references a higher layer.
2. **No Claude Code in core tests** — if a session, a hook, or CLAUDE.md shows up in a core test, a concept has leaked.
3. **Locked core vocabulary** — an identifier that violates the [glossary.md](glossary.md) mapping table gets rejected in review.

## Domain model

The core knows exactly four concepts.

- **worker** — an entity that can process work. The core doesn't know whether it's a human, an AI session, or a script. It reports which topics it can own, and has an online/offline status.
- **topic** — the target of work. To the core it's an opaque hierarchical path. The core knows exactly two things about it: (1) it's a path split by a separator — `/`, settled during M1 implementation — (2) prefix matching is possible (a worker that owns `iyulab` becomes a candidate for an item in front of `iyulab/ironhive`, but not `iyulab2/x`; segment boundaries matter) — and, like every identifier the core compares, it is matched case-insensitively ([ADR-0021](decisions/ADR-0021-case-insensitive-identity.md)). The meaning of each segment is defined by the application — the core doesn't know the word "repo."
- **item** — a single unit of work waiting to be processed. Created in front of a topic; it's fine for it to have no owner at creation time.
- **claim** — a worker picking up an item to become its owner. **Exclusive** — concurrent claims by multiple workers aren't allowed (single-claim only, settled — [ADR-0002](decisions/ADR-0002-four-layer-architecture.md)).

A `worker id`, `requester`, `assignee`, and `topic` are all one identity class: strings whose only
job is to say whether two references mean the same thing. Comparison folds two kinds of variation
before checking equality — case ([ADR-0021](decisions/ADR-0021-case-insensitive-identity.md)) and a
declared **alias**, a caller-asserted "these two spellings name the same identity"
([ADR-0022](decisions/ADR-0022-identity-alias.md)). Both fold at comparison time only; storage
always keeps whichever spelling was actually written.

A `claim` is a pull a worker performs on its own. An admin's "force-assign" is just an entry point at the application/permission layer where the admin triggers that same `claim` on the worker's behalf — the core doesn't need a separate `assign` concept.

## Item identity

```
id:  string   # canonical identifier — a UUID v4, assigned at creation, never reused.
seq: integer  # short numeric alias for the same item — assigned at creation, from a single
              # global counter, in creation order, never reused (even after delete). Purely a
              # human/token-friendlier reference; `id` remains authoritative and unchanged.
```

Every operation that takes an item id accepts either form interchangeably — a caller-supplied
value that parses as a bare integer (with or without a leading `#`) is resolved against `seq`;
anything else is assumed to already be the canonical `id`. Resolution happens once, inside the
store, so every layer above it (HTTP, `docket-mcp`, `docket-cc`) needs no awareness of the
distinction. Full decision rationale: [ADR-0016](decisions/ADR-0016-item-seq-alias.md).

## Item state schema

```
state: open | claimed | resolved | closed
resolution: null | done | duplicate | wontfix | invalid | blocked | deferred   # only has a value when closed
```

Two additional transitions exist alongside the forward path above, both moving an item backward
into the assignee's hands: `reject` (`resolved -> claimed`, requester-initiated, requires a
reason) and `reopen` (`closed -> claimed`, clears `resolution`, requires a reason — or
`closed -> open` when the item was closed before anyone ever claimed it, since there is no
assignee to hand it back to). Neither introduces a new `state` value — see
[ADR-0012](decisions/ADR-0012-item-reject-reopen-transitions.md) for why.

`resolved` marks the point where "the ball is back in the requester's court" — the worker has reported that it handled the item, and the requester confirms and closes it (same meaning as RESOLVED in Bugzilla/Jira). `resolution` is a separate field from `state`, and admin operations map onto it as follows:

| Admin operation | resolution |
|---|---|
| Remove (clean up an item created by mistake) | `invalid` |
| Merge (consolidate a duplicate item) | `duplicate` |
| Force-close (close an item that's become irrelevant) | `wontfix` |
| Force-approve (admin confirms completion when `claim`/`submit` never happened) | `done` |
| Requester approval (normal completion) | `done` |

There's no `expired` here — the policy for automatic claim expiry / automatic stall-closing hasn't been decided yet. It gets added once that policy is settled.

`force-approve` and requester approval both write `resolution = done` — telling them apart means
reading the lifecycle comment's op name (`"force-approve"` vs `"approved"`), not `resolution` alone.
See [ADR-0017](decisions/ADR-0017-item-force-approve.md).

`block`/`defer` close an item from any pre-closed state with `resolution = blocked`/`deferred` —
unlike the five admin operations above, these are not admin overrides but the normal, fully
reversible way a *worker* parks an item that cannot currently progress (a concrete external
dependency, or unproven cross-consumer demand). `reopen_item` is the way back for either, the same
as for an admin close. Both require a `reason`, recorded as the lifecycle comment verbatim (like
`reject`/`reopen`, unlike the bare op-name marker the admin closes record) — it's the only record
of *why*, load-bearing for whoever later decides to reopen. See
[ADR-0018](decisions/ADR-0018-blocked-deferred-resolution.md).

Full decision rationale: [ADR-0003](decisions/ADR-0003-item-state-schema.md).

## Item requester/assignee/turn

```
requester: string | null   # who this item is being worked for. Optional, set at creation —
                             # or after the fact via `PATCH /items/{id} {"requester": "…"}`
                             # (also exposed as the `set_item_requester` MCP tool), for
                             # backfilling items filed before a requester was known, or
                             # repairing one that drifted.
assignee:  string | null   # the current holder (was `owner`) — set by claim, checked by submit.
turn: requester | assignee | null   # derived from `state`, never stored — see below.
```

`turn` makes the "ball is back in the requester's court" language above literal: `assignee` while
`open` (unclaimed, but still squarely waiting on the assignee side to look at it and act — the same
party as once it's claimed) or `claimed` (the assignee's turn to act), `requester` while `resolved`
(the requester's turn to approve), `null` only while `closed` (done — nobody's turn). It's computed
from `state` at read time, not a fourth stored field, so it can never drift out of sync with the
state it describes.

Full decision rationale: [ADR-0010](decisions/ADR-0010-item-from-to-turn.md) /
[ADR-0011](decisions/ADR-0011-requester-assignee-naming.md).

`open` is computed the same way: `state != closed`. Same treatment as `turn` — never stored,
always derived, for the same never-drift reasoning. See
[ADR-0012](decisions/ADR-0012-item-reject-reopen-transitions.md).

## Archiving and deletion

```
archived_at: integer | null   # epoch millis; null unless archived
```

Independent of `state` — an item in any workflow state can be archived. `list_items`/
`search_items` exclude archived items by default; pass `archived: true` to browse only the
archive. No `unarchive` operation exists yet (additive if the need shows up).

`delete_item` (HTTP-only, `DELETE /items/{id}`, no MCP tool) permanently removes an item and its
tags/comments. Distinct from `remove_item`, which closes an item with `resolution = invalid` but
keeps a permanent record — `delete_item` leaves no trace at all, and takes no `author`/`reason`
for that reason.

Full rationale: [ADR-0013](decisions/ADR-0013-item-archive-and-delete.md).

## Question

Separately from items (`task`), there's a request type with no state machine that fails immediately — if there's no owner, it fails on the spot and never lands on the board. See [vision.md](vision.md) S3. Whether it lives in the core or only at layer 3 is still undecided.

## Storage engine

**Starting with a single-instance SQLite** ([ADR-0004](decisions/ADR-0004-sqlite-storage.md)). There's no hard performance constraint ([principles.md](principles.md)), and the assumed scale (single owner, multiple machines) doesn't need more than this right now.

## Public scope and license

**Fully public, single monorepo.** License is Apache-2.0. All four layers are developed in one public repo (no plan to split into separate repos). From this decision onward, internal-context scrubbing discipline applies to every commit, doc, and line of code. Rationale: [ADR-0005](decisions/ADR-0005-public-scope.md).

## Extension points

What the layer split actually opens up.

- **Other agent runtimes** → add layer 3 only (layer 2 is already general-purpose)
- **Human workers** (someone picking up items from a phone) → extend layer 4, core unchanged
- **Other topic conventions** (systems that aren't repos) → add only an application convention
- **`aims`** → an incident event becomes an item, its own agent becomes the worker. Core stays as-is

## MCP-exposure rule

Not every `docket-core` HTTP operation becomes a `docket-mcp` tool. An operation is exposed to
MCP when a worker can safely call it on its own judgment — reversible, or destructive only to
something disposable (a claim, a tag). An operation stays off MCP when it is irreversible against
durable history, or represents an admin/human value judgment about an item's disposition:
`remove`, `merge`, `force-close`, `force-approve`, `delete`, and `redact` (both `redact_item` and
`redact_comment`) all stay HTTP-only under this rule, each with a console button for the human
operator MCP exclusion is routing around. `redact` in particular is the most irreversible operation
in the whole surface — even `delete` at least removes a value along with everything referencing
it, where `redact` deliberately leaves the item or comment in place with one field gone, which is
exactly the shape a worker could reach for as a casual "oops, let me fix that" and get catastrophically
wrong (see [ADR-0023](decisions/ADR-0023-redact-item-and-comment.md)).
`PATCH /items/{id}` is exposed field by field — `set_item_requester`, `set_item_assignee`,
`set_item_topic` — because each corrects one item's own metadata, which carries none of the blast
radius above. The alias endpoints (`POST`/`GET`/`DELETE /aliases`) stay HTTP-only: declaring an
alias is a judgment about identity that changes who may `approve`/`reject` every item under it,
past and future, not metadata on one item (see
[ADR-0022](decisions/ADR-0022-identity-alias.md)). **Reading is not declaring**, which is why
`GET /identities` *is* exposed (`list_identities`) although it concerns the same identity class —
the rule turns on what an operation can change, not on which concept it touches, and an
enumeration changes nothing (see
[ADR-0025](decisions/ADR-0025-identity-enumeration-and-drift.md)).
`GET /identities/candidates` has no tool of its own for a different reason — not risk, but scope:
it reaches a worker through `list_items`/`search_items`' `report_drift` flag, which keeps it
answering about an identity the caller already named instead of becoming a standing report.
Unlike the buttoned operations above, neither `redact` nor the alias endpoints have a console
surface today — HTTP (or a raw client atop it) is the only way to reach them.
`claim`/`submit`/`approve`/`reject`/`reopen`/`archive`/`block`/`defer` are all MCP tools. `force-approve` in particular must stay off MCP — exposing it there would let
a worker approve its own (or another item's) completion without ever reaching `resolved`, exactly
the shortcut the `claim → submit → approve` split exists to prevent (see
[ADR-0017](decisions/ADR-0017-item-force-approve.md)). `block`/`defer` are MCP tools precisely
because they don't carry that risk — either is fully undone by `reopen_item`, and neither lets a
worker claim a disposition (`done`/`duplicate`/`wontfix`/`invalid`) it hasn't earned; it can only
say "I can't move this forward right now," which is exactly the kind of thing a worker is trusted
to judge for itself (see [ADR-0018](decisions/ADR-0018-blocked-deferred-resolution.md)). See
[ADR-0013](decisions/ADR-0013-item-archive-and-delete.md).
