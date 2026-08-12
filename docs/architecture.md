Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Architecture

Covers only Type-1 (hard-to-reverse) decisions. Everything else is deliberately left open until implementation forces a choice.

## Four layers

| # | Name | Responsibility | Knows its consumer? |
|---|---|---|---|
| 1 | `docket-core` | Headless work queue. Worker/topic/item/claim/stall/budget. HTTP API + real-time stream | No |
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
- **topic** — the target of work. To the core it's an opaque hierarchical path. The core knows exactly two things about it: (1) it's a path split by a separator — `/`, settled during M1 implementation — (2) prefix matching is possible (a worker that owns `iyulab` becomes a candidate for an item in front of `iyulab/ironhive`, but not `iyulab2/x`; segment boundaries matter). The meaning of each segment is defined by the application — the core doesn't know the word "repo."
- **item** — a single unit of work waiting to be processed. Created in front of a topic; it's fine for it to have no owner at creation time.
- **claim** — a worker picking up an item to become its owner. **Exclusive** — concurrent claims by multiple workers aren't allowed (single-claim only, settled — [ADR-0002](decisions/ADR-0002-four-layer-architecture.md)).

A `claim` is a pull a worker performs on its own. An admin's "force-assign" is just an entry point at the application/permission layer where the admin triggers that same `claim` on the worker's behalf — the core doesn't need a separate `assign` concept.

## Item state schema

```
state: open | claimed | resolved | closed
resolution: null | done | duplicate | wontfix | invalid   # only has a value when closed
```

`resolved` marks the point where "the ball is back in the requester's court" — the worker has reported that it handled the item, and the requester confirms and closes it (same meaning as RESOLVED in Bugzilla/Jira). `resolution` is a separate field from `state`, and admin operations map onto it as follows:

| Admin operation | resolution |
|---|---|
| Remove (clean up an item created by mistake) | `invalid` |
| Merge (consolidate a duplicate item) | `duplicate` |
| Force-close (close an item that's become irrelevant) | `wontfix` |
| Requester approval (normal completion) | `done` |

There's no `expired` here — the policy for automatic claim expiry / automatic stall-closing hasn't been decided yet. It gets added once that policy is settled.

Full decision rationale: [ADR-0003](decisions/ADR-0003-item-state-schema.md).

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
