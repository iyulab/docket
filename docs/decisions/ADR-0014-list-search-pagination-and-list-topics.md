Status: v0 alignment snapshot | 2026-08-20 | settled

# ADR-0014: `limit`/`offset` on `list_items`/`search_items`, and a `list_topics` primitive

## Context

`list_items`/`search_items` return every matching row, `body` included, with no bound. Reported
by a consumer as the single most frequent, most consistent friction observed across four
independent downstream projects: an unfiltered or single-topic query routinely exceeds the
MCP tool-output token cap, forcing the caller to fall back to iterating known topic names one at a
time (itself only possible because the caller already knows the topic list — there is no API to
discover it) or slicing a dumped-to-file response by hand. The same absence of a bound was
independently confirmed on the HTTP path: an unfiltered `GET /items` against a live instance
returned 502KB across 77 items, and `docket-console`'s only data path is a 5-second poll of that
same endpoint — every tick re-downloads the full, ever-growing history, `body` included, to draw a
kanban board that only needs `id`/`title`/`state`/`tags`.

Both symptoms trace to the same root cause: no caller — MCP or HTTP — has any way to ask for a
bounded slice of the result. This is squarely inside `docket-core`'s own responsibility layer, not
a consumer-domain concept: any caller of a project at this scale, immediately, hits the same wall
(`라이브러리 한계 = 개선 기회`).

## Options considered and trade-offs

**Pagination shape:**

- **Reject — cursor/keyset pagination**: the correct shape for infinite-scroll UX at scale, but
  `list_items`/`search_items` order by `updated_at`, a field mutated by nearly every write this
  project has — a keyset cursor over a frequently-mutated sort key needs tie-break/staleness
  handling this project has no evidenced need for yet, and the reported problem is "a single
  response exceeds a token cap", not "browsing many pages interactively". Complexity ahead of
  demand — YAGNI.
- **Accept — `limit`/`offset`** (adopted): the simplest shape that solves the reported problem.
  Both are optional; omitting either preserves a bounded default rather than today's unbounded
  behavior — see defaults below.

**Where the slice is taken:**

- **Reject — inside `Store::list_items`/`search_items` (SQL `LIMIT`/`OFFSET`)**: the HTTP handler
  in `main.rs` applies `topic_scope`/`assignee`/`requester` as a *second*, in-process filter pass
  after the store call returns (pre-existing, unrelated to this ADR). A SQL-level `LIMIT` would be
  computed against the *pre*-filter row set, so the page a caller receives could be smaller than
  `limit` (or empty) even when more matching rows exist — silently wrong pagination. Store-level
  pagination is a defensible design in isolation, but two independent slicing layers acting on
  different filter stages is not.
- **Accept — a final slice applied in the HTTP handler, after every filter** (adopted): correct
  regardless of which filters combine, and the currently list_items/search_items row counts don't
  justify SQL-level `LIMIT` as a performance necessity yet. `Store` methods are unchanged.

**Defaults and cap:**

- **Accept — `limit` default 50, hard-capped at 200; `offset` default 0** (adopted): today's
  behavior (unbounded) is exactly what caused this issue, so "unbounded unless specified" is
  rejected outright — the point of this ADR is that a caller gets a bounded response *without*
  having to know to ask for one. 50 keeps most single-topic queries (the reported failure mode)
  well under the observed overflow thresholds (63K–92K characters at 20–70 items); the 200 cap
  exists so a caller who does pass an explicit large `limit` still can't reproduce the original
  unbounded-response failure by accident.
- **Given up for now**: a `limit`/`offset` alone does not bound worst-case response size when
  individual `body` fields are large (50 items × a few KB of body each can still be substantial) —
  a field-selection/"summary" mode (own `body` out of list responses) would close that gap
  completely and would also directly fix the `docket-console` polling-cost finding, but is left to
  a follow-up rather than folded into this ADR's scope (see Re-open trigger).

**Pagination metadata — response shape:**

- **Reject — wrap the HTTP body in an envelope (`{items: [...], total, limit, offset}`)**: changes
  the response shape for every existing HTTP caller of an already-shipped, if young, endpoint —
  more disruptive than the problem requires.
- **Accept — keep the HTTP body a bare `Item[]`; add total counts as an `X-Total-Count` response
  header** (adopted): zero shape change for existing callers; a caller that wants `total` reads the
  header, one that doesn't is unaffected. Ordinary REST convention (cf. GitHub API's
  `Link`/pagination headers) rather than an invented shape.
- **MCP layer is a separate decision**: MCP tool results have no header channel, so `docket-mcp`'s
  `list_items`/`search_items` tools wrap their own output as `{items, total, limit, offset}` —
  reading the HTTP response's `X-Total-Count` header and re-shaping it into the one output format
  an MCP caller can actually see. The two layers legitimately differ in shape because their
  transports differ, not because of inconsistent taste.

**`list_topics` — a new primitive:**

- **Accept — `GET /topics` / MCP tool `list_topics`, returning `[{topic, count}]`** (adopted):
  the thing that would let a caller stop guessing topic names to page through one at a time,
  requested directly by the same consumer report above. Same `archived_at IS NULL` default
  convention as
  `list_items`/`search_items` (an archived item's topic still exists but its count shouldn't
  suggest active work sits there); no separate `archived` toggle is added to `list_topics` itself —
  a topic doesn't disappear from the vocabulary just because every item filed under it happens to
  be archived, so there's nothing here for that toggle to usefully exclude, unlike an item list.

## Decision

```
GET /items?...&limit=50&offset=0        # limit: default 50, clamp to [1, 200]; offset: default 0
                                          # applied after every existing filter (topic_scope/
                                          # assignee/requester included), response body shape
                                          # unchanged (still a bare Item[]); adds response header
                                          # X-Total-Count: <row count before the limit/offset slice>

GET /topics                              # -> [{ "topic": string, "count": integer }], ordered by
                                          #    count DESC, ties broken by topic ASC. Excludes items
                                          # with archived_at set (same default as list/search).

docket-mcp: list_items/search_items gain optional limit/offset params (same defaults/cap),
tool output becomes { items: ItemDto[], total, limit, offset } instead of a bare array.
docket-mcp: new list_topics tool, output [{ topic, count }].
```

No new `state`/`resolution` value, no schema/table change — `list_topics` is a `GROUP BY` over the
existing `items` table.

## Consequences

**Gained**: every `list_items`/`search_items` response, MCP or HTTP, is bounded by default instead
of unbounded — the reported failure mode (a broad or even single-topic query exceeding the MCP
token cap) cannot recur from an unbounded response alone. A caller can discover the topic
vocabulary directly instead of enumerating candidate names it has to already know. `docket-console`
gains an accurate `X-Total-Count` it can use later if pagination reaches the console (not scoped
here — see Re-open trigger).

**Given up**: a caller (MCP or HTTP) that genuinely wants "everything, unbounded" no longer gets it
in one call — it must page via `offset` (up to the 200-per-page cap). Response size is still
unbounded in the pathological case of large `body` fields at the cap (see "summary mode",
deferred). `docket-console`'s per-tick payload is unaffected by this ADR alone — it doesn't yet
pass `limit`/`offset`, and the endpoint's default (`limit=50`) would silently truncate its board
past 50 items if the console isn't updated in the same change — **implementation must update the
console to either page or explicitly request `limit` above its practical board size**, and this is
tracked as this ADR's first follow-up item precisely to avoid that regression.

## Re-open trigger

If a "summary" / field-selection mode becomes a real, evidenced need (the console polling-cost
finding, or a `body`-heavy response still exceeding the token cap at `limit=50`), that is a small,
additive follow-up (a `fields`/`summary` query param excluding `body`), not a re-open of this ADR's
reasoning. If cursor-based pagination becomes a real need (an actual infinite-scroll UI, not just
avoiding a token cap), revisit the "reject — cursor/keyset" option above with real usage evidence.
