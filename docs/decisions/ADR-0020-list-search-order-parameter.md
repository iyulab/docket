Status: v0 implementation | 2026-08-24 | implemented

# ADR-0020: `order` parameter on `list_items`/`search_items`

## Context

[ADR-0014](ADR-0014-list-search-pagination-and-list-topics.md) bounded `list_items`/`search_items`
with `limit`/`offset`, but left the sort direction exactly as it already was — a hardcoded
`ORDER BY updated_at DESC` in `Store::list_items`/`search_items`
(`crates/docket-core/src/storage.rs:385,905`) with no caller-facing option. That fact was not even
documented until a live audit ([docket-works#26](https://github.com/iyulab/docket-works/issues/26))
found a caller had to read the SQL to learn it, then work around the absence of an ascending option
by pulling every open item and sorting/computing age client-side. The docs-only half of that issue
(stating the fixed-`desc` contract in `docs/usage.md` and the MCP tool descriptions) was already
shipped before this ADR; this ADR is the "improvement" half the issue also asked for, previously
held back pending real friction (`docket-works/ROADMAP.md`'s YAGNI note) and reopened here on
direct instruction (the same escalation path [ADR-0019](ADR-0019-approve-reject-requester-match.md)
used).

The friction is real and structural, not cosmetic: "find the longest-untouched item" is a natural
query against a field this project already tracks (`updated_at`) and already sorts by — a caller
that wants the tail of that same ordering has no way to ask for it directly today, only to page to
the end of the *head*-first ordering via `offset`, which requires first learning `total` and doing
the arithmetic. Any caller of `list_items`/`search_items` at this project's scale hits the same wall
the moment it needs staleness rather than recency — squarely inside `docket-core`'s own
responsibility layer (`라이브러리 한계 = 개선 기회`), not a consumer-domain concept.

## Options considered and trade-offs

**Shape of the control:**

- **Reject — a separate `sort` field naming the column** (`sort=updated_at`, forward-looking to a
  future second sortable column): no second sortable column exists or is asked for anywhere in this
  project (`created_at` is the only other timestamp, and nothing has asked to sort by it) —
  building a column-selection axis now is speculative. YAGNI.
- **Accept — `order: asc | desc`, direction only, same fixed column (`updated_at`)** (adopted): the
  minimal shape that answers the actual reported need ("oldest first" vs "newest first"). Matches
  the issue's own suggested name.

**Default and backward compatibility:**

- **Accept — `order` optional, default `desc`** (adopted): identical to today's fixed behavior when
  omitted — every existing caller (MCP or HTTP) is unaffected. An unrecognized value falls back to
  the default silently, the same treatment `tag_match` already gets (`ListItemsQuery`/
  `SearchItemsQuery` in `main.rs`) rather than a `400` — consistent with this project's established
  "unrecognized filter value degrades to default, doesn't error" convention (`docket-works` issue
  #18's "미인식 필터 키 무응답 수용" finding).

**Where the direction is applied:**

- **Reject — post-filter, in-process reversal in the HTTP handler** (mirroring where ADR-0014 put
  `limit`/`offset`): ADR-0014 moved slicing to the handler specifically because the handler applies
  a *second* filter pass (`topic_scope`/`assignee`/`requester`/`mine`) after the store call — a
  SQL-level `LIMIT` computed before that second pass could under-fill a page. Sort direction has no
  such interaction: reversing the *order* of a fully-filtered set gives the identical set in the
  opposite order regardless of which layer flips it, so there is no correctness reason to duplicate
  ADR-0014's split here.
- **Accept — inside `Store::list_items`/`search_items`, by choosing `ASC`/`DESC` in the `ORDER BY`
  clause** (adopted): one line change at the exact spot the fixed ordering already lives, no new
  layer, and the ordering established here is what the handler's subsequent filters and
  `skip(offset).take(limit)` slice operate over — get this right once, upstream of every later
  operation, rather than sorting again downstream.

**Whether `docket-mcp` gets the parameter in the same change:**

- **Accept — yes, same batch** (adopted, no alternative seriously considered): the deployment split
  between `docket-core`(HTTP, deployed immediately via `publish-docket-core.sh`) and
  `docket-mcp`/`docket-cc` (GitHub-Releases-only, launcher-fetched) already burned this project once
  — `docket-works/CLAUDE.md`'s "배포 게이트" section, added after a real incident (2026-08-20) where
  `docket-core` served new reject/reopen/archive tools that `docket-mcp` had no way to call. Adding
  `order` to `docket-core` alone and leaving `docket-mcp` for later would reproduce exactly that gap.

## Decision

```
Store::list_items(topic, state, archived, order: SortOrder) -> Vec<Item>
Store::search_items(topic, state, tags, tag_match, query, archived, order: SortOrder) -> Vec<Item>
  # ORDER BY updated_at {ASC | DESC}, chosen by `order` — same clause, same position, only the
  # keyword changes. Every other filter and the caller-side limit/offset slice (ADR-0014) are
  # unaffected — they operate on whichever ordering `order` selects.

domain::SortOrder { Asc, Desc }             # as_str()/parse(), same shape as TagMatch
  Desc is the enum's Default — matches today's fixed behavior when `order` is omitted.

GET /items?...&order=asc|desc               # optional, default `desc` (today's behavior).
                                              # Unrecognized value falls back to `desc`, same
                                              # treatment as an unrecognized `tag_match`.

docket-mcp: list_items/search_items gain optional `order: "asc" | "desc"` param, forwarded
  verbatim as the HTTP query parameter above. Tool descriptions updated to drop the "fixed, no
  ascending option" language and state the real, now-selectable, default-`desc` behavior.
```

No new `state`/`resolution` value, no schema/table/migration — a hardcoded SQL keyword becomes a
caller-selected one at the same call site.

## Consequences

**Gained**: "find the oldest-untouched items" becomes one call (`order=asc`, optionally with
`limit`) instead of pulling every row and sorting client-side — the exact reported friction
(`docket-works#26`) is closed for MCP and HTTP callers alike, in the same release. The already-fixed
"desc" contract stated in `docs/usage.md`/tool descriptions stays true by construction: it is still
the default, just no longer the only option.

**Given up**: nothing removed or narrowed — this is additive over ADR-0014's shape, and every
existing caller (omitting `order`) sees byte-identical behavior to before this ADR.

**Implemented** (2026-08-24, same day as this decision): `domain::SortOrder` (`Asc`/`Desc`,
`as_str`/`parse`, `Default = Desc`) added alongside `TagMatch`, same shape. `Store::list_items`/
`search_items` take it and select the `ORDER BY updated_at` keyword from it (`storage.rs`).
`docket-core`'s `GET /items` gained `order` (parsed the same permissive way as `tag_match`,
defaulting on any unrecognized value). `docket-mcp`'s `list_items`/`search_items` tools gained the
same `order` param, forwarded verbatim as the HTTP query parameter, in the same change —
`docs/usage.md` §4 and both tool descriptions updated to state the real, now-selectable behavior.
Workspace version `0.14.0` → `0.15.0` (minor, additive). Verified: 3 new `docket-core` unit tests
(`domain`'s parse/default round-trip, `list_items`/`search_items` asc-reverses-desc), 1 new HTTP
integration test (`order=asc` flips ordering; an unrecognized `order` value still returns `200` with
the default ordering, not a `400`), 1 new `docket-mcp` integration test proving `order` is forwarded
end to end through both tools — `cargo test --workspace` clean (0 failed) throughout.

## Re-open trigger

If a real need for sorting by a column other than `updated_at` (e.g. `created_at`, to find the
*oldest-filed* rather than *oldest-touched* item) shows up with actual evidence, revisit the
rejected `sort=<column>` shape above rather than bolting a second direction-only flag onto a second
column.
