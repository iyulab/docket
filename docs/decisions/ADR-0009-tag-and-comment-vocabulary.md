Status: v0 implementation | 2026-08-16 | implemented

# ADR-0009: `tag` and `comment` join the core vocabulary

## Context

`principles.md` P-1 closes the core's vocabulary to exactly `worker, topic, item,
claim, body, stream, budget`, mechanically enforced by `glossary.md`'s core/application
mapping table. Adding tagging and commenting to items means introducing two new nouns —
`tag` and `comment` — into `docket-core`'s schema and API surface. Widening a
closed vocabulary list is a Type-1 (hard-to-reverse) decision under this project's
decision discipline: once consumers (MCP tools, HTTP clients, `docket-cc`) start
calling `add_tags`/`search_items`/`add_comment`, removing the concept again means a
breaking API change, not a local edit. It is decided here, before implementation,
rather than left to accrete as an unexamined side effect of a feature PR.

Tags and comments follow the same schema-and-API pattern (`item_tags`, and
`item_comments` as an append-only log with no edit/delete). Both are decided together
here because they're the same kind of addition to the same closed list, and splitting
them into two ADRs would just repeat this argument twice.

## Options considered and trade-offs

- **Reject — keep tags/comments entirely in the application layer**: an application
  adapter could shadow-store tags/comments in a side table or file keyed by item id,
  outside `docket-core`. This preserves the closed vocabulary literally, but every
  consumer of a generic work queue needs *some* way to label items for later
  retrieval and to leave a threaded note on one — this is not `docket-cc`-specific
  or `iyulab`-specific the way `scopePath` or a tenant id would be. Pushing it to the
  application layer would mean every consumer reimplements the same join table and
  the same "don't lose history, only append" logic that a generic work queue should
  own once. That is exactly the shape P-1's own cost note describes ("every time
  there's a temptation to drop a \[client\]-specific feature straight into the core,
  you have to keep paying the cost of pushing that concept up and translating it").
  The difference here is that tag/comment are not client-specific — they're
  workflow-generic, so the cost of exclusion is paid by every consumer, not saved by
  any of them.
- **Reject — encode tags as structured data on `body`**: e.g. a
  `<!-- tags: a,b -->` convention inside the markdown body. Rejected because it
  reintroduces exactly the "state buried in file text, not polled" failure mode this
  work exists to eliminate: a status marker appended to a file's tail is invisible to
  any reader who doesn't reopen and scroll to the bottom of that specific file, unlike
  a queryable field a caller can search. A searchable, indexed field is the point;
  encoding it back into `body` throws that away.
- **Accept — add `tag` and `comment` as core vocabulary (adopted)**: `item_tags` and
  `item_comments` become sibling tables to `items`, exposed through `Store` methods
  and MCP/HTTP endpoints the same way `claim`/`submit`/`approve` already are. Tags
  are stored as opaque strings — core never interprets a tag's content, the same way
  it never interprets a topic segment (P-1's existing discipline for `topic`).
  Comments are append-only with no edit/delete API, mirroring the project's own
  issue-draft convention of not deleting history, only appending resolution.

## Decision

`tag` and `comment` are decided here as core vocabulary, alongside
`worker, topic, item, claim, body, stream, budget`. Both are generic work-queue
primitives:

- A **tag** is an opaque, caller-defined string attached to zero or more items,
  many-to-many, mutation is idempotent (`add_tags`/`remove_tags`), and search can
  filter by tag with `any`/`all` set semantics (`TagMatch`). **Status: implemented** —
  this task adds `item_tags`, `Store::{add_tags, remove_tags, list_tags, search_items}`,
  and `tag` is reflected in `principles.md` P-1's vocabulary list and `glossary.md`'s
  mapping table as of this same commit.
- A **comment** is an append-only, timestamped note attached to one item, authored by
  a worker id or caller-supplied string. No update or delete operation — corrections
  are new comments, not edits to old ones. **Status: implemented** — this task adds
  `item_comments`, `Store::{add_comment, list_comments}`, and `comment` is reflected in
  `principles.md` P-1's vocabulary list and `glossary.md`'s mapping table as of this
  same commit.

Neither concept carries any domain-specific meaning (no `severity`, `scopePath`,
`tenantId`, or similar baked into the schema) — the string content of a tag and the
body text of a comment are exactly as opaque to core as an item's `body` already is.

## Consequences

**Gained**: any consumer of `docket-core` — not only `docket-cc` — gets labeling and
threaded notes as queue primitives, without reimplementing a join table and an
append-only log per consumer. This directly replaces file-convention coordination
patterns that rely on status folders and notes buried in file text: a tag plus a
comment is a queryable, single-source-of-truth substitute for both.

**Given up**: the closed vocabulary list grows from 7 entries to 9, which is exactly
the cost P-1 warns about — every future proposal to add a core concept now has two
recent precedents to point to, so the bar for the next one has to be held at the same
place (cross-consumer genericity, not any single consumer's convenience) or the list
drifts open by precedent instead of by argument.

## Re-open trigger

If a future addition to `item_tags` or `item_comments` needs a consumer-specific
concept (e.g. a fixed tag namespace, or comment visibility scoping) that only one
consumer needs, that addition does not extend this ADR — it goes through the
application adapter layer instead, per the "domain boundary" principle in the
upstream-library-extension policy. This ADR only covers the generic primitives
decided above.
