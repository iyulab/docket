Status: v0 implementation | 2026-10-30 | decided, not yet implemented

# ADR-0029: The core owns the read contract

## Context

The people reading docket's answers are mostly language-model agents, and for them the scarce resource is
context. Over a month of real sessions about half of everything docket returned was list rows, and of those
rows more than half had already been returned earlier in the same session or described work that was finished;
a conversation was always read whole; the two whole-table listings (topics, identities) were returned in
full on every call. The tool definitions an agent loads before it asks anything were themselves about as
large as everything docket answered in a typical session.

The first fixes for this were made in `docket-mcp`: trimming rows, bounding a page by size, reading the tail of
a thread, narrowing a table by name. That is the wrong place for them. `docket-mcp` is one client among
several (`docket-cc`, the console, direct HTTP callers), the shaping has to be re-implemented by each, and only
the server can say with authority how many rows matched, how many were held back, and where the next read
should start. Where to start reading, how much to read and how densely to read it are properties of the
answer, and the answer is the server's.

## Decision

1. **Every list-shaped read accepts the same three kinds of control.**
   - *Where to start*: `after` / `since` / `offset` (as the endpoint already has them) and `tail` (the newest N).
   - *How much*: `limit` (rows) and `max_chars` (characters of the rows as serialised).
   - *How densely*: `fields` (the named fields only, `id` always kept) — and, where the endpoint lists names,
     `q` (a case-insensitive substring of the name or an alias).
2. **The server says what it did.** The body of a list stays a bare array. The metadata travels in response
   headers, as `X-Total-Count` already does ([ADR-0014](ADR-0014-list-search-pagination-and-list-topics.md)):
   `X-Omitted` (matching rows held back by `limit` or `max_chars`) and `X-Next-Offset` (where to continue).
   `X-Shaped: 1` is sent by every endpoint that honours these controls, so a client can tell a server that
   shaped the answer from an older one that ignored the parameters.
3. **`max_chars` always returns at least one row**, so a budget smaller than a row cannot leave a caller with an
   empty page and no way forward.
4. **The core knows rows, fields and characters, never tokens.** Converting an agent's token allowance into
   characters is the adapter's job ([P-1](../principles.md), [ADR-0027](ADR-0027-budget-leaves-the-core-vocabulary.md)).
5. **No default cap on HTTP.** An unshaped request returns what it always returned, so the console and existing
   callers are unaffected. An adapter that serves agents sets its own defaults (a row cap on whole-table
   listings, for instance) and passes them as parameters.
6. **Older servers ignore unknown query parameters**, which is why an adapter checks for `X-Shaped` and, when it
   is missing, applies the same shaping to the page itself rather than silently returning an unshaped answer.
7. **Endpoints.** Items (list and search): `fields`, `max_chars` and the existing `updated_after`. An item's
   comments: `since`, `tail`, `max_chars`. Topics and identities: `q`, `limit`. Topic comments
   ([ADR-0028](ADR-0028-topic-comments.md)) are born with the comment form of these controls.

## What this does not decide

- A compact row view (identifying a row by `seq` alone), cursor tokens for items, ranking by relevance, and any
  summarising. Each is taken on evidence of what agents actually re-read.
- A per-comment sequence number as the exact "after" cursor for comments. It belongs with the storage change in
  ADR-0028, which rebuilds the comments table anyway; until then comments are read `since` a time.
