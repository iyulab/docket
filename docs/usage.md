# Using docket

This is the single reference for actually *operating* docket — as an MCP tool-calling agent, as a
plain HTTP client, or as a human. Read this end to end and you have everything needed to register,
file, discover, and complete work through it. For *why* docket is shaped this way, see
[vision.md](vision.md) / [principles.md](principles.md) / [architecture.md](architecture.md) instead —
this document only covers *how*.

## 0. The model, in one paragraph

docket is a work-queue: **workers** register the **topics** they own, **items** get filed against a
topic, and a worker **claims** an item (exclusive — only one worker wins), does the work, **submits**
it, and the item's requester **approves** it closed. Everything is a thin HTTP API
(`docket-core`) fronted by two equivalent clients: MCP tools (`docket-mcp`) for an agent inside an
MCP-capable session, or raw HTTP for anything else (`docket-console`, `curl`, scripts).

| Term | Meaning |
|---|---|
| `worker` | Something that can own topics and claim items — typically one Claude Code (or similar) session |
| `topic` | A named queue items are filed against — competing consumers, one worker wins each item (Kafka-topic-plus-consumer-group semantics, not pub-sub fan-out) |
| `item` | A single unit of work |
| `claim` | A worker pulling an open item to itself, exclusively |
| `state` | `open → claimed → resolved → closed` — the workflow stage. `reject` and `reopen` move an item backward — onto `claimed`, or onto `open` when `reopen` hits an item nobody ever claimed (§4) |
| `resolution` | Why an item closed: `done` (requester approval, or admin `force-approve`) / `duplicate` (merge) / `wontfix` (force-close) / `invalid` (remove) / `blocked` (`block_item`) / `deferred` (`defer_item`) — `force-approve` and a normal `approve` both write `done`; tell them apart via the lifecycle comment's op name (`"force-approve"` vs `"approved"`), not `resolution` alone. `blocked`/`deferred` are the odd ones out: not a final disposition, just "parked" — `reopen_item` is the way back, same as any other closed item. See [ADR-0018](decisions/ADR-0018-blocked-deferred-resolution.md) |
| `open` | `true` while `state != closed`, else `false` — derived from `state`, not stored, same treatment as `turn`. See [ADR-0012](decisions/ADR-0012-item-reject-reopen-transitions.md) |
| `requester` / `assignee` / `turn` | `requester` is who the item is for, `assignee` is the current holder (was `owner`), `turn` says whose hand it's in right now — derived from `state`, not stored, and only changes when `state` does. `add_comment` never moves `state`; only `claim_item`/`submit_item`/`approve_item`/`reject_item`/`reopen_item`/`block_item`/`defer_item` do, so an assignee narrating a whole workflow through comments alone leaves `turn` exactly where it started. See [ADR-0010](decisions/ADR-0010-item-from-to-turn.md) / [ADR-0011](decisions/ADR-0011-requester-assignee-naming.md). `list_items`/`search_items`'s `mine=<worker id>` filter (§4) is the query-side equivalent of "whose turn" — it matches exactly the items where it is that worker's turn, as either assignee or requester |
| `archived_at` | `null` unless archived, else the epoch-millis timestamp it was archived at — independent of `state`, caller-set, hides an item from default listings. See [ADR-0013](decisions/ADR-0013-item-archive-and-delete.md) |
| `tag` | An opaque, caller-defined string on an item — docket never interprets it |
| `comment` | An opaque, append-only note on an item — no edit/delete, corrections are new comments |

Full vocabulary mapping (including the terms your application layer should translate *from* before
they reach docket): [glossary.md](glossary.md).

## 1. Get a `docket-core` server

Someone runs one `docket-core` process; every client (MCP or HTTP) talks to it over the network. If
you're joining an existing setup, get its base URL from whoever runs it — that's the `DOCKET_CORE_URL`
used everywhere below. To run your own (e.g. for local development):

```bash
cargo run -p docket-core
```

Binds `127.0.0.1:8420` by default (override with `DOCKET_BIND`/`DOCKET_PORT`), opens/creates a SQLite
file at `docket.db` in the working directory (override with `DOCKET_DB_PATH`). No authentication
exists yet (see §9) — keep it off any network you don't trust until M4 adds one.

## 2. Install the clients

```bash
curl -fsSL https://raw.githubusercontent.com/iyulab/docket/main/scripts/install.sh | sh
```
```powershell
irm https://raw.githubusercontent.com/iyulab/docket/main/scripts/install.ps1 | iex
```

This installs `docket-mcp`/`docket-cc` as small launchers (default `$HOME/.local/bin` on
Linux/macOS, `%LOCALAPPDATA%\docket\bin` on Windows — override with `DOCKET_INSTALL_DIR`). Each
launcher checks GitHub Releases for its own worker binary on every run and caches the result, so once
installed you never manually update either one again.

"Every run" means every time the launcher process is *started* — for `docket-mcp` that's once per
MCP client session, since the client spawns it as a long-lived stdio server rather than re-invoking
it per call. A release that lands while a session is already running won't reach that session; the
launcher also re-checks once an hour in the background for as long as the session stays up, and
prints a one-line warning to stderr the first time it finds a newer release than the one the session
started with — restart the session (or the MCP connection) to pick it up.

`docket-cc`, by contrast, is invoked fresh on every `SessionStart` (§7) rather than staying up for
the whole session, so its launcher's release check is never stale by more than one session start. When
that check is the one that *just* downloaded a release no launcher on the machine had cached before,
`docket-cc hook`'s own output — which a `SessionStart` hook does surface to you, unlike an
already-running `docket-mcp` server's stderr — adds a line naming the new version and prompting you to
restart any other open sessions to pick it up.

## 3. Register `docket-mcp` with an MCP client

Claude Code:

```bash
claude mcp add docket-mcp -s user -e DOCKET_CORE_URL=http://<host>:<port> -- <path-to-docket-mcp>
```

Use the **absolute path** to the installed binary (`<install-dir>/docket-mcp` on Linux/macOS,
`<install-dir>\docket-mcp.exe` on Windows), not a bare command name — an MCP client spawns the
server directly, without a shell, so it never resolves `PATH`/`PATHEXT` the way an interactive shell
would; a bare name silently fails to connect. `-s user` registers it for every project on the
machine, not just the one you happen to run the command from.

Any other MCP client that reads stdio-server JSON config:

```json
{
  "mcpServers": {
    "docket": {
      "command": "<path-to-docket-mcp>",
      "env": { "DOCKET_CORE_URL": "http://<host>:<port>" }
    }
  }
}
```

Without a client-level registration, you can also run it directly (`DOCKET_CORE_URL=... docket-mcp`,
or `cargo run -p docket-mcp` from source) and point any stdio-capable MCP client at that process.

## 4. Tool reference

Every tool is a thin wrapper over one HTTP call — the "HTTP" column is the exact equivalent if you're
talking to `docket-core` directly instead (base path `/api/*`, or unprefixed; both work identically).
Errors come back as a **tool-level error** you can see and react to (e.g. retry `list_items` after
losing a claim race), never a silent protocol failure.

Every `item_id` below accepts either the item's canonical id or its short numeric alias — `seq`,
e.g. `142` or `#142` — returned alongside `id` on every item ([ADR-0016](decisions/ADR-0016-item-seq-alias.md)).
Both forms resolve to the same item; `id` stays canonical everywhere (stored references like
`merge_item`'s `duplicate_of_id` tag are unaffected).

**Identifiers compare case-insensitively.** A worker id, `requester`, `assignee` and `topic` name
the same thing whichever case they are written in, so `mine=iyulab/Filer` and `mine=iyulab/filer`
return the same items, and `approve_item` matches a `requester` that drifted in case rather than
failing. **Storage keeps what was written** — an item filed with `iyulab/Filer` reads back as
`iyulab/Filer`, so grouping returned values by string can still show two spellings for one identity.
`register_worker` is the exception worth knowing: a drifted id lands on the registration that
already exists and the response returns that row's spelling, which is how a session whose derived id
drifted learns the canonical one. ASCII only; tags are deliberately *not* folded (they stay opaque
to core). See [ADR-0021](decisions/ADR-0021-case-insensitive-identity.md).

**Beyond case, an admin can *declare* two spellings the same identity** — a rename, a short form,
an org migration — via `POST /aliases` (below). A declared alias folds the same way case does,
including in `approve_item`/`reject_item`'s requester check, but only once declared: an undeclared
lookalike is still refused, so this never widens who may act on an item by accident. See
[ADR-0022](decisions/ADR-0022-identity-alias.md).

| Tool | Params | HTTP | Notes |
|---|---|---|---|
| `register_worker` | `id?`, `topics[]` | `POST /workers` | Call once per session. `topics` are prefixes — see §5. `id` **resolvable-required** at this tool layer (cycle-58, same pattern as `claim_item` above) — omit it to use this session's `DOCKET_WORKER_ID`; a tool-level error only if neither is present |
| `get_worker` | `id?` | `GET /workers/{id}` | The only way to positively confirm a worker is registered — see the read/write not-found note below. 404s on an unregistered id. `id` **resolvable-required** at this tool layer (HD-18, cycle-59, same pattern as `register_worker` above) — omit it to look up this session's own registration via `DOCKET_WORKER_ID`; a tool-level error only if neither is present. **Direct HTTP callers must percent-encode the id**: a worker id is conventionally `org/repo`, and an unencoded `/` makes the path two segments, which misses this single-segment route entirely (`GET /workers/iyulab%2Fdocket`, not `/workers/iyulab/docket`). The MCP tool does this for you — see [docket-works#36](https://github.com/iyulab/docket-works/issues/36), where it did not and every `org/repo` id was unlookupable. An unmatched path now answers a JSON 404 like any other, rather than the console's HTML shell |
| `create_item` | `topic`, `title`, `body?`, `tags[]?`, `requester?` | `POST /items` | **Call `search_items` first** to avoid filing a duplicate. `requester` is who this item is being worked for — optional (see [ADR-0010](decisions/ADR-0010-item-from-to-turn.md) / [ADR-0011](decisions/ADR-0011-requester-assignee-naming.md)) |
| `set_item_requester` | `item_id`, `requester`, `author?` | `PATCH /items/{id} {"requester","author"}` | Corrects `requester` on an existing item — **whether or not it already has one**. Two cases, both in scope since ADR-0011: *backfilling* an item filed before a requester identity was available (or one a migration left blank), and *repairing* an identity that drifted afterwards — a typo, a renamed repo, two consumers spelling the same identity differently. The second is what `approve_item`/`reject_item` point here for when their requester match fails ([ADR-0019](decisions/ADR-0019-approve-reject-requester-match.md) names it the mitigation): correct the item, never retry the call under the wrong spelling. State-unrestricted (works on a closed item too — this corrects metadata, not a workflow transition) and idempotent — setting the value it already has changes nothing and records nothing. `requester` must not be blank. A real change writes a lifecycle comment naming both values (`requester: (unset) → acme/widget`), atomically with the update, the same way `reject_item`/`reopen_item`/`block_item`/`defer_item` record theirs — this is the one edit that can move an item between two parties, so who changed it and from what belongs in the thread. `author` is **resolvable-required** at the MCP tool layer (omit it to use this session's `DOCKET_WORKER_ID`); a direct HTTP caller may omit it and gets docket-core's `"unknown"` default, the same asymmetry `add_comment` has. Does not cover `assignee`/`turn`/`title`/`body`. `topic` is a separate field on the same
`PATCH /items/{id}` request but has no MCP tool of its own — see the `PATCH /items/{id}` note
below |
| `list_items` | `topic?`, `state?`, `assignee?`, `requester?`, `topic_scope?`, `mine?`, `archived?`, `limit?`, `offset?`, `summary?`, `order?`, `expand_related?` | `GET /items?topic=&state=&assignee=&requester=&topic_scope=&mine=&archived=&limit=&offset=&summary=&order=&expand_related=` | `topic_scope=<worker id>` is how a worker discovers its own queue (§5) — matches by topic jurisdiction, not who currently holds any given item. `assignee`/`requester` match the item's current assignee/requester — case-insensitively, like every identifier comparison ([ADR-0021](decisions/ADR-0021-case-insensitive-identity.md)). `mine=<worker id>` is the one-shot "what should I be looking at" filter — `assignee=<id>` OR (`requester=<id>` AND `state=resolved`, i.e. waiting on *your* decision: approve it, or answer what the assignee asked and hand it back with `reject_item`) OR (`state=open` AND unclaimed AND under a topic `<id>` is registered for), so you don't have to know to run and merge those three yourself; ANDs with every other filter here, same as `assignee`/`requester` individually — see §5 and [docket-works#35](https://github.com/iyulab/docket-works/issues/35) for the third case. `archived` defaults to `false` (today's behavior); `true` browses only the archive — see §4's archiving note. **Tool result is `{items, total}`, not a bare array** — `limit` defaults to 50, capped at 200; `total` is the row count before that cap, so a `total` above `items.length` means page further with `offset` ([ADR-0014](decisions/ADR-0014-list-search-pagination-and-list-topics.md)). Over HTTP the body stays a bare `Item[]`; `total` comes back as the `X-Total-Count` header instead. `summary=true` nulls every returned item's `body` — use it when you only need enough of each row to decide which item (if any) to fetch in full next via `get_item`/`GET /items/{id}`, which is unaffected. **Rows are ordered by `updated_at`, `desc` (most-recently-touched first) by default** — pass `order=asc` to get the *oldest*-untouched items directly, without paging to the tail via `offset`; an unrecognized `order` value falls back to `desc` rather than erroring, same treatment an unrecognized `tag_match` gets below (see [ADR-0020](decisions/ADR-0020-list-search-order-parameter.md)). `expand_related=true` applies `get_item`'s `related` expansion to every row in the returned page (not the unpaged total) — same both-directions semantics, same caveat about a seq-alias-tagged reference not showing up as `referenced_by` ([docket-works#33](https://github.com/iyulab/docket-works/issues/33)) |
| `search_items` | `query?`, `tags[]?`, `tag_match?`, `topic?`, `state?`, `assignee?`, `requester?`, `topic_scope?`, `mine?`, `archived?`, `limit?`, `offset?`, `summary?`, `order?`, `expand_related?` | `GET /items?q=&tag=&tag=&tag_match=&topic=&state=&assignee=&requester=&topic_scope=&mine=&archived=&limit=&offset=&summary=&order=&expand_related=` | `query` full-text matches title+body+comments — matched word-by-word (each word independently, not as one exact adjacent phrase), so word order doesn't matter and a query word also prefix-matches a token carrying a suffix it doesn't have (e.g. a stemmed or CJK-particle-suffixed form). `tag_match` is `any` (default) or `all`; `assignee`/`requester`/`topic_scope`/`mine`/`archived`/`limit`/`offset`/`summary`/`order`/`expand_related`/response shape/ordering same as `list_items` above — full-text search and an ownership filter combine in one call |
| `get_item` | `item_id`, `expand_related?` | `GET /items/{id}?expand_related=` | Fetch one item by id — the way to resolve an id from a shared link or a comment into its current state/resolution/requester/assignee/turn/tags/body. Unaffected by `list_items`/`search_items`' `summary` mode (`body` always included), and returns archived items too (a direct id lookup, not a list query). `expand_related=true` also resolves the item's `related:<id>` tags (see [glossary.md](glossary.md)) into a `related` field, both directions (`references` this item's own tags name; `referenced_by` — another item's tag naming this one back, only found if that tag used the canonical id, not a seq alias). Defaults to `false` — the `related` key is then entirely absent, not `null`/`[]` ([docket-works#33](https://github.com/iyulab/docket-works/issues/33)) |
| `claim_item` | `item_id`, `worker_id?` | `POST /items/{id}/claim {"worker_id"}` | `open → claimed`. Exclusive — loser gets a tool-level error, not a crash. **Call this before starting any work** — it's the only thing that moves `turn` off its `open` default; narrating the work through `add_comment` alone never does. `worker_id` **resolvable-required** at this tool layer (HD-16/HD-17, cycle-57) — omit it to use this session's `DOCKET_WORKER_ID` (`docket-cc-launcher` injects it every session); a tool-level error only if neither is present |
| `submit_item` | `item_id`, `worker_id?`, `reason?` | `POST /items/{id}/submit {"worker_id","reason"}` | `claimed → resolved` — **the only transition that moves `turn` to the requester**, and it means "the assignee can't take this further, the requester decides what happens next". That covers finished work *and* work waiting on an answer only the requester has; **submit in both cases** ([ADR-0010](decisions/ADR-0010-item-from-to-turn.md)'s 2026-09-08 update). Holding a question in `claimed` instead hides it: `mine` matches items assigned to the requester, `resolved` items they filed, and unclaimed items in their topics — never one the assignee is holding — so the question sits unread in the thread. `reason` is optional, recorded as a lifecycle comment atomically with the transition; put the question in it. The requester answers with `reject_item`, whose required `reason` carries the answer back. Only the current assignee may submit. `worker_id` resolvable-required, same as `claim_item` above |
| `approve_item` | `item_id`, `author?` | `POST /items/{id}/approve {"author"}` | `resolved → closed`, `resolution=done`. The requester's sign-off. `author` **resolvable-required** at this tool layer (HD-16/HD-17, cycle-57) — omit it to use this session's `DOCKET_WORKER_ID`; a tool-level error only if neither is present. Separately, **`author` must match the item's `requester` if one is set** — a core-enforced invariant, not just an MCP-layer requirement (ADR-0019, mirrors `submit_item`'s `assignee` match on the other side of the handshake); passes through unchanged when `requester` is `null`. A mismatch is a `conflict` distinguishing "wrong state" from "wrong requester", pointing at `set_item_requester` for a drifted identity. Direct HTTP callers get the exact same requester check — it is not MCP-layer-only |
| `reject_item` | `item_id`, `reason`, `author?` | `POST /items/{id}/reject {"reason","author"}` | `resolved → claimed`. The requester handing the turn back to the assignee — **not** done yet. Rework is one use; **answering a question the assignee submitted is an equally ordinary one**, since `reason` is what carries the answer (ADR-0010's 2026-09-08 update). The name reads as a verdict, but the transition is a turn handoff — see that update on why no third state was added for it. `reason` is required (recorded as a comment, atomically with the state change). `author` resolvable-required, and must match the item's `requester` if one is set, same as `approve_item` above |
| `reopen_item` | `item_id`, `reason`, `author?` | `POST /items/{id}/reopen {"reason","author"}` | `closed → claimed` (or `→ open`, if the item was closed before anyone ever claimed it), clears `resolution` back to `null`. For a close that turns out to have been premature or mistaken, or for un-parking a `block_item`/`defer_item`. `reason` is required, same as `reject_item`. `author` resolvable-required, same as `approve_item` above |
| `block_item` | `item_id`, `reason`, `author?` | `POST /items/{id}/block {"reason","author"}` | `(any pre-closed state) → closed`, `resolution=blocked`. For a concrete external dependency nothing on either side can move forward on right now (no access to a paywalled standard, waiting on a third party). Not an admin override — any worker calls this on its own judgment, same as `reject_item`/`reopen_item`. `reason` is required, recorded as the lifecycle comment verbatim — it's the only record of *why* for whoever reopens it later. `author` resolvable-required, same as `approve_item` above. See [ADR-0018](decisions/ADR-0018-blocked-deferred-resolution.md) |
| `defer_item` | `item_id`, `reason`, `author?` | `POST /items/{id}/defer {"reason","author"}` | `(any pre-closed state) → closed`, `resolution=deferred`. Same shape as `block_item`, for a softer reason than a hard external block (e.g. cross-consumer demand not yet proven). See [ADR-0018](decisions/ADR-0018-blocked-deferred-resolution.md) |
| `archive_item` | `item_id` | `POST /items/{id}/archive` | Hides the item from default `list_items`/`search_items` results (still fully queryable with `archived: true`). Idempotent, no data lost, works from any `state`. No `unarchive` yet |
| `add_tags` / `remove_tags` | `item_id`, `tags[]` | `POST`/`DELETE /items/{id}/tags {"tags"}` | Idempotent both ways. If you use the `related:<id>` convention (see [glossary.md](glossary.md), `get_item`/`list_items`'s `expand_related`), tag with the target's **canonical id**, not its `seq` alias — `add_tags` never rewrites a tag's value, so a seq-alias-tagged reference resolves forward but is never found by the other item's `referenced_by` lookup (a deliberate limitation, not a bug — kept this way so tags stay opaque to core, see [principles.md](principles.md) P-1) |
| `list_tags` | `topic?` | `GET /tags?topic=` | **Call before tagging** to reuse existing vocabulary instead of inventing a synonym. Returns `{tag, count}[]`, most-used first |
| `list_topics` | — | `GET /topics` | **Call before `list_items(topic=…)`** to discover topic names instead of guessing/enumerating them. Returns `{topic, count}[]`, most-populated first; excludes topics whose items are all archived, same as an all-archived `list_items` query would |
| `add_comment` | `item_id`, `body`, `author?` | `POST /items/{id}/comments {"author","body"}` | `author` is **resolvable-required** at this tool layer (same treatment as `claim_item`/`submit_item`/`approve_item`/`reject_item`/`reopen_item` above, HD-16/HD-17, cycle-57) — omit it to use this session's `DOCKET_WORKER_ID`; a tool-level error only if neither is present, so a comment never silently lands docket-core's `"unknown"` fallback. Direct HTTP callers hitting `POST /items/{id}/comments` outside this tool (e.g. a script) still get docket-core's own optional-`author`-defaults-to-`"unknown"` behavior — this requirement is MCP-layer only. **Never changes `state` or `turn`** — a fully-narrated workflow told entirely through comments leaves the item exactly where `claim_item`/`submit_item`/etc. last left it |
| `list_comments` | `item_id` | `GET /items/{id}/comments` | Chronological, append-only |

`reject_item`/`reopen_item` both send an item backward into the assignee's hands and both require a
`reason` — the difference is only which edge they start from. Normally both land on `claimed`; the
one exception is reopening an item that was closed before anyone claimed it, which lands on `open`
instead, since there is no assignee to hand it back to (it's the assignee's turn either way — see
`turn` below). A round trip through both looks like:

```
submit_item(item_id, worker_id)              # claimed  -> resolved
reject_item(item_id, reason="missing the empty-topic case")
                                              # resolved -> claimed, resolution stays null
# … assignee does more work …
submit_item(item_id, worker_id)              # claimed  -> resolved (again)
approve_item(item_id)                        # resolved -> closed,  resolution = done

reopen_item(item_id, reason="the fix regressed a different case")
                                              # closed   -> claimed, resolution: done -> null
```

Neither transition adds a new `state` value — both land back on an ordinary existing one, and
*why* the item bounced lives in the comment `reason` records, not in `state` itself. See
[ADR-0012](decisions/ADR-0012-item-reject-reopen-transitions.md).

The same two transitions carry a **question and its answer**, which is the other thing a round trip
is for. An assignee who needs the requester to decide something submits with the question as
`reason`; the requester answers with `reject_item`, whose `reason` is the answer:

```
submit_item(item_id, worker_id, reason="need the failing input to reproduce — can you attach it?")
                                              # claimed  -> resolved, turn -> requester
reject_item(item_id, reason="attached below; it's the empty-topic case")
                                              # resolved -> claimed,  turn -> assignee
```

Do **not** stay in `claimed` while waiting for that answer. `resolved` does not assert the work is
finished — it asserts whose turn it is (see `turn`, below) — and it is the only state that puts the
item in front of the requester at all: `mine` matches items assigned to them, `resolved` items they
filed, and unclaimed items in their topics, but never one the assignee is still holding. A question
left in `claimed` is therefore invisible to every query the requester runs, and waits on someone who
has no way to know they are being waited on. The cost of the alternative is that `reject_item` reads
as a verdict in the history when it was really an answer — accepted deliberately, because the
transitions already exist and a third state would have to be threaded through `turn`, `mine`,
`resolution` and the console to buy only a better label. See
[ADR-0010](decisions/ADR-0010-item-from-to-turn.md)'s 2026-09-08 update.

`GET /items/{id}` (fetch one item by id) has an MCP tool — `get_item`, table above. Prefer it over
`list_items`/`search_items` plus a filter whenever you already have the id (e.g. from a shared link
or a comment) — it's one call instead of a list-and-scan, and unlike the list tools it also reaches
an archived item without passing `archived: true`. `GET /workers/{id}` (fetch one worker by id) has
the same kind of MCP tool — `get_worker`, table above — it's the only way to positively confirm a
worker is registered, since every list-style filter answers an unregistered id the same as a
registered one with no matches (see the read/write not-found note below).

`PATCH /items/{id} {"requester": "…", "topic": "…", "author": "…"}` corrects `requester` and/or
`topic` on an item that already exists — at least one of the two is required, both may be given
together. **The two fields differ in MCP exposure**: `requester` has an MCP tool
(`set_item_requester`, table above); `topic` does not. State-independent for both (works on a
closed item too — this corrects metadata, not a workflow transition). Rejects a blank value with
`400`, a missing item with `404`.

- **`requester`** is the only way to give an item a requester after creation (`requester` is
  normally set once at creation, ADR-0010/ADR-0011) — see `set_item_requester`'s row above for the
  backfill/repair distinction.
- **`topic`** corrects an item filed against the wrong topic — the item-level counterpart to
  declaring an alias (below): an alias is for a spelling people actually use across many items,
  declaring one for a single mistyped item would make that typo permanent schema, so this is the
  one-off fix instead. Unlike `requester`, it has no MCP tool — the same admin-only reasoning as
  the alias endpoints and the admin operations below, though unlike those admin operations neither
  `topic` correction nor declaring an alias has a console button yet — both are HTTP-only, full
  stop. There is still no way to edit `title`/`body` after creation.

When both are given, they are applied **in sequence — `requester` first, then `topic` — not as one
transaction**: if the `topic` half then fails validation, the `requester` half has already been
committed, the response is a flat `400` naming which field failed, and a `GET` on the item is the
only way to confirm what landed. Each successful field change is recorded as its own lifecycle
comment naming the old and new value.

**Declaring an alias** says two spellings name the same identity — a rename, a short form, an org
migration — not something a folding rule can derive on its own the way case is folded automatically
([ADR-0021](decisions/ADR-0021-case-insensitive-identity.md)). This has no MCP tool, for the same
reason `force-approve` doesn't ([architecture.md](architecture.md#mcp-exposure-rule)): it changes
who may `approve_item`/`reject_item`, not on one item but on every item under that identity, on a
server with no authentication yet — an admin judgment about disposition rights, not something a
worker can safely decide for itself in the course of its own work (see
[ADR-0022](decisions/ADR-0022-identity-alias.md)'s MCP-exposure note). Once declared, the effect is
retroactive and immediate — no migration, nothing to backfill — because every comparison
(`assignee`/`requester`/`topic_scope`/`mine`, `list_topics`, and the `approve`/`reject` requester
check) resolves through the alias table at the moment it runs.

| HTTP | Notes |
|---|---|
| `POST /aliases {"alias","canonical"}` | Declares `alias` the same identity as `canonical`. Idempotent for re-declaring the same pair. `409` if `alias` is already declared pointing elsewhere, or if either side of the pair would form a chain (an alias can't itself be a canonical, and a canonical can't itself be an alias — resolution is always one hop). `400` for a blank or self-referential pair |
| `GET /aliases?canonical=` | Lists declared aliases, newest first; `canonical` narrows to one identity's variants (folds case, like every identifier comparison) |
| `DELETE /aliases?alias=` | Withdraws a declaration — not destructive to any item, it just stops folding into the canonical. Takes `alias` as a **query parameter, not a path segment**, since an alias is `org/repo`-shaped and an unencoded `/` in a path segment misses the route entirely (the same failure `GET /workers/{id}` shipped with, see the `get_worker` row above). `404` if not declared |

**`GET /topics/candidates?topic=`** flags a topic that is probably mistargeted — a typo, or a topic
worth declaring an alias for. Two conditions, both exact, no similarity scoring: the given `topic`
is **unserved** (no registered worker's topics match it), and some other topic that already has
items **is** served and shares its last `/`-delimited segment. Always advisory — sharing a last
segment is common and legitimate (`acme/widget` and `other-org/widget` are unrelated), so nothing
here rejects or rewrites anything; it only surfaces a candidate for `set_item_topic` (one-off fix)
or `put_alias` (standing declaration) to act on. Returns `{"unserved": bool, "candidates": [topic,
...]}`. `create_item` calls this automatically and, when it fires, attaches the result as a
`topic_advisory` string in the tool result — there is no separate MCP tool for it. See
[ADR-0022](decisions/ADR-0022-identity-alias.md) for why exact segment matching is the right amount
of guessing and edit distance is not.

Four more HTTP-only operations close an item early, bypassing the normal
`claimed → resolved → closed` path — they're console/admin actions (`docket-console` exposes them as
buttons), not worker actions, so there's no MCP tool for them. All four are assignee-agnostic and valid
from any state except `closed` (unlike `approve`, they don't require reaching `resolved` first):

| HTTP | resolution | Meaning |
|---|---|---|
| `POST /items/{id}/remove {"author"}` | `invalid` | The item was a mistake — never should have been filed |
| `POST /items/{id}/merge {"duplicate_of_id", "author"}` | `duplicate` | Consolidated into another item |
| `POST /items/{id}/force-close {"author"}` | `wontfix` | No longer relevant, closed without being done |
| `POST /items/{id}/force-approve {"author"}` | `done` | An admin confirms the work is actually complete even though `claim_item`/`submit_item` were never called — e.g. a worker only narrated completion through `add_comment` (§4's `add_comment`/`claim_item` note) and `approve_item` now rejects with `cannot approve: item is open/claimed` since the item never reached `resolved`. See [ADR-0017](decisions/ADR-0017-item-force-approve.md). |

All four take an optional `author`, recorded as a comment alongside the close, exactly like
`approve_item` above — it defaults to `"unknown"` if omitted. Unlike `approve_item`/`reject_item`,
none of the four check `author` against `requester`: they exist specifically as an override valve
for cases the normal requester handshake doesn't cover, so they stay assignee- and
requester-agnostic (ADR-0019). The request body (for
`remove`/`force-close`/`force-approve`) may be omitted entirely (a bodiless `POST` is accepted and
takes the same default). `merge` is the one exception: `duplicate_of_id` is **required, non-blank**
— `resolution = duplicate` alone can't say duplicate of what, so `merge` also atomically tags the
item `duplicate-of:<id>` (a free-form-tag reference, not a schema column — see
[ADR-0015](decisions/ADR-0015-merge-duplicate-of-reference.md)). No referential check that
`duplicate_of_id` names a real item — tags stay opaque, caller-defined strings to the store.

> **`submit_item` is the only door into `resolved` — repeated comments never substitute for it.**
> A worker that reports through `add_comment` alone (however many times, and whether it is reporting
> completion or asking a question) leaves `state` exactly where it was; `approve_item` stays
> unreachable until `submit_item` actually runs, and until then the item is not in front of the
> requester at all.
> If a worker skips `claim_item`/`submit_item` entirely, the requester's only way to close the item
> as done is the admin-side `force-approve` above — there is no worker-side or MCP-side path.

> **`remove_item` is not `delete_item` — do not confuse the two.**
>
> - **`POST /items/{id}/remove`** (table above) *closes* the item: `state → closed`,
>   `resolution → invalid`. The item, its tags, and its comments all still exist and are still
>   queryable — this is how you mark "this should never have been filed" while keeping a
>   permanent, traceable record of that fact.
> - **`DELETE /items/{id}`** (`delete_item`, no MCP tool — reachable over plain HTTP or from
>   `docket-console`'s detail view, no `author`/`reason` params since there is no item left
>   afterward to attach either to) *destroys* the item outright: the row,
>   its tags, and its comments are all gone. Nothing is left to query. This is for a mistaken or
>   throwaway item where no trace should remain at all — not for routine cleanup.
>
> If you want a record of *why* something went away, use `remove_item`. Reach for `delete_item`
> only when you specifically want zero record to remain. Full rationale:
> [ADR-0013](decisions/ADR-0013-item-archive-and-delete.md).

Separately, `archive_item` (`POST /items/{id}/archive`, MCP tool) is not a deletion at all — it
sets `archived_at` and hides the item from default `list_items`/`search_items` results, but the
item, its tags, and its comments remain fully intact and reachable with `archived: true`. Archiving
is routine, low-stakes hygiene a worker can do on its own judgment; deleting is not — see the
[architecture.md](architecture.md#mcp-exposure-rule) MCP-exposure rule for why one is an MCP tool
and the other isn't.

An `Item` looks like:

```json
{
  "id": "…", "topic": "iyulab/docket", "title": "…", "body": null,
  "state": "open", "resolution": null, "requester": null, "assignee": null, "turn": "assignee",
  "open": true, "archived_at": null,
  "tags": [], "created_at": 1734000000000, "updated_at": 1734000000000
}
```

`requester`/`assignee`/`turn` are the two-party handoff — `requester` is who this item is being
worked for, `assignee` is the current holder (was `owner`), `turn` is derived from `state` and tells
you whose hand it's in right now: `"assignee"` while `open` (unclaimed, but still squarely waiting on
the assignee side to look at it) or `claimed` (the assignee's turn to act), `"requester"` while
`resolved` (the requester's turn to decide — approve, or answer and hand back), `null` only
while `closed` (done — nobody's turn). See
[ADR-0010](decisions/ADR-0010-item-from-to-turn.md) /
[ADR-0011](decisions/ADR-0011-requester-assignee-naming.md).

`turn` says whose hand the item is in *within docket* — it does not say whether the actual
bottleneck is inside docket at all. An item can sit at `turn: "assignee"` because the assignee is
genuinely waiting to act, or because the assignee is blocked on something docket has no visibility
into (an external approval, a purchase, another person entirely) — the two look identical from
`turn` alone. A `blocked`-style tag plus a comment explaining why is the way to make that
distinction visible to anyone reading the item, since there is no separate field for it.

`open` is `state != closed`, computed the same way as `turn` — never stored, so it can never drift
out of sync with `state`. `archived_at` is `null` unless the item was archived; it's independent of
`state` (an item in any workflow state can be archived) and only affects whether default
`list_items`/`search_items` calls surface the item.

Errors are `{"error": "<message>"}` with `404` (not found), `409` (state conflict — e.g. `"cannot
claim: item is claimed"`), or `500` (server-side failure). A `claim`/`submit`/`approve`/`reject`/
`reopen`/`remove`/`merge`/`force-close`/`force-approve` call that loses a race or targets the wrong
state always comes back `409`, never `500` — that's the signal to re-`list_items` and try something
else rather than treat it as a bug. `archive_item` and `delete_item` are state-unrestricted (valid
from any state) so this doesn't apply to either.

**A *list/search* filter never 404s on a non-matching or unregistered reference — a call that
targets one specific known resource by id does.** `list_items`/`search_items`/`list_comments`/
`list_tags` answer any filter that matches nothing (an unknown `topic`, `assignee`, `requester`,
`topic_scope`/`mine` worker id, or `item_id`) with an empty result, the same way a database query
does — there is no "does this reference exist" check on a filter. `GET /items/{id}` (`get_item`) and
`GET /workers/{id}` (`get_worker`), and every mutate call (`create_item`/`claim_item`/`submit_item`/
`approve_item`/`reject_item`/`reopen_item`/`archive_item`/`add_comment`/`add_tags`/`remove_tags`/
`delete_item`/the three admin close operations), target one specific item or worker by id and 404
when it doesn't exist — fetching or acting on one named thing has nothing sensible to do with "no
such reference" other than fail. Rely on this instead of treating an empty
list as ambiguous: it always means "no matches", never "the thing you filtered by doesn't exist" —
there's nothing else it could mean, since a filter doesn't look that up in the first place.
`list_items(topic_scope=<id>)`/`list_items(mine=<id>)` in particular can't tell you whether `<id>` is
a registered worker — `topic_scope` treats "unregistered" the same as "registered, no matching
topics" (both: empty result), and `mine` doesn't touch registration at all (it only ever compares
against `assignee`/`requester` on items, which exist independently of any worker record) — call
`get_worker` directly if you need to know whether an id is actually registered.

## 5. The worker loop

This is the pattern an agent repeats:

1. **Once per session**: `register_worker(id, topics)` — `topics` are prefixes (`"iyulab"` owns every
   topic starting `iyulab/…`, exact match or `/`-delimited prefix, compared case-insensitively;
   see `topic_matches` in [glossary.md](glossary.md) and
   [ADR-0021](decisions/ADR-0021-case-insensitive-identity.md)).
   Re-registering under a differently-cased id updates the registration you already have and returns
   its canonical spelling — it does not create a second worker. **A session responsible for several
   repositories should register every topic it owns, not just the one it happened to start in** —
   `register_worker` takes `topics` as an array, and `topic_scope`/`mine` (step 2) then cover all of
   them in one call instead of one per repository. `docket-cc topic --all` (§6) is what produces that
   full list for an umbrella-and-submodules tree — pass its output straight into `topics[]`. Passing
   only the topic of the repository you happened to run `topic` from is not a missing feature, it's
   an easy-to-miss step: the under-registered session's `topic_scope`/`mine` queries simply return
   fewer rows than they should, silently, for every topic left out.
2. **Check what you need to act on**: `list_items(mine=<your id>)` — one call covers everything:
   an item claimed in a prior session, one waiting on your approval as requester, *and* any `open`
   (unclaimed) item under a topic you're registered for. That third case was added by
   [docket-works#35](https://github.com/iyulab/docket-works/issues/35) — before it, a caller had to
   separately remember `topic_scope=<id>&state=open` (old step 3, now folded in here) on top of
   `assignee=<id>` and `requester=<id>&state=resolved`, and a worker with several topics could
   silently miss a freshly-filed item in one of them if it forgot that third query. `mine`
   deliberately excludes `closed` items — once closed, nobody's turn
   ([ADR-0010](decisions/ADR-0010-item-from-to-turn.md)), so it's no longer something you "hold" or
   need to look at. That also means `mine` does not surface an item that reached `closed` without
   your involvement as requester — the four admin overrides (`remove`/`merge`/`force-close`/
   `force-approve`) and `block_item`/`defer_item` are all state-unrestricted and requester-agnostic by
   design (they exist specifically to bypass the normal `claim → submit → approve` handshake — see
   [ADR-0012](decisions/ADR-0012-item-reject-reopen-transitions.md)/
   [ADR-0017](decisions/ADR-0017-item-force-approve.md)/
   [ADR-0018](decisions/ADR-0018-blocked-deferred-resolution.md)/
   [ADR-0019](decisions/ADR-0019-approve-reject-requester-match.md)), so none of them wait on you the
   way `approve_item`/`reject_item` now do. If you want to audit for exactly this — an item you filed
   that closed without your approval — run `list_items(requester=<your id>, state="closed")`
   periodically; it is the complete, if unfiltered-by-resolution, answer to "what closed while I
   wasn't looking", and combining it with `mine` gives full requester-side coverage across every
   non-terminal and terminal state. `topic_scope=<your id>` (without a `state` filter) is still worth
   running on its own when you want full visibility across *every* state in your jurisdiction, not
   just what's actionable for you right now — `mine` is deliberately the narrower "what do I act on"
   view, not a replacement for browsing a topic wholesale.
3. **Before filing something new**: `search_items(query=…)` — check it doesn't already exist.
4. **Take an item**: `claim_item(item_id, worker_id)`. If it 409s, someone else got there first — go
   back to step 2.
5. Do the actual work.
6. **Hand it back**: `submit_item(item_id, worker_id, reason?)` — moves it to `resolved`, which
   means the requester's turn, not necessarily "done". Do this both when the work is finished and
   when you need a decision only the requester can make; put the question in `reason`. Staying in
   `claimed` while you wait hides the item from every query the requester runs (ADR-0010's
   2026-09-08 update).
7. The requester (whoever wanted the item done — may be a different worker, or a human via
   `docket-console`) calls `approve_item(item_id)` once satisfied, closing it — or answers with
   `reject_item(item_id, reason=…)`, which hands the turn back to you with the answer attached.

Tags and comments are asynchronous side-channels on top of this loop — attach them whenever relevant,
they don't gate any state transition.

## 6. Topic derivation (`docket-cc topic`)

Don't hand-type topics for repo-shaped work — derive them:

```bash
cd path/to/some/repo && docket-cc topic
# iyulab/some-repo
```

Reads no env vars, talks to nothing — pure filesystem lookup. Walks up from the current directory to
the nearest `.git`, reads its `origin` remote's `org/repo`. A submodule's own `.git` stops the walk at
the submodule (resolves to the submodule's own remote, not the umbrella's); a `git worktree` resolves
through to the repository it was created from. The whole repository is one topic by default, however
many packages live inside it. Drop a `.docket/topic` file (its first non-empty line, plain text) in
any ancestor directory to override the derivation entirely.

`git worktree` and `claim` are orthogonal: a worktree isolates the filesystem, `claim` isolates
ownership. Running several sessions in parallel via `git worktree` still means they all resolve to the
same topic and safely race for items in it via `claim` — the worktree doesn't need to (and shouldn't
try to) also carve out a separate topic per worktree.

Working across an umbrella and its submodules (like this repository's own `docket-works`/`docket`
pair) means the umbrella root and each submodule resolve to *different* topics by design (above) — a
session that only registers the one it happened to run `topic` from misses the others, silently
(a `topic_scope`/`mine` filter on the missed topic just returns fewer rows, no error). `topic --all`
covers the whole tree in one call, reading `.gitmodules` recursively (an umbrella-of-umbrellas
included) rather than requiring you to enumerate submodules by hand:

```bash
cd path/to/some/umbrella && docket-cc topic --all
# iyulab/some-umbrella
# iyulab/some-submodule
```

One topic per line, in `.gitmodules` declaration order — pass straight into `register_worker`'s
`topics[]`. An uninitialized submodule (listed in `.gitmodules` but never `git submodule update`d) is
skipped, not guessed at.

## 7. File projection & Claude Code hook (`docket-cc`)

An alternative to MCP tool calls: project every item a worker owns onto local `.md` files instead.

```bash
DOCKET_CORE_URL=http://<host>:<port> DOCKET_WORKER_ID=<id> DOCKET_CC_ROOT=~/.docket cargo run -p docket-cc
```

One-shot and write-only — rerun to pick up changes. Layout mirrors the topic path:
`<root>/iyulab/docket/<item-id>.md`. `DOCKET_CC_ROOT` defaults to a platform user-data directory
(deliberately outside any repo).

`docket-cc hook` runs the same projection, then prints a plain-text summary of currently open items
(nothing, if there are none) — meant for a `SessionStart` hook:

```json
{
  "hooks": {
    "SessionStart": [{
      "matcher": "startup",
      "hooks": [{
        "type": "command",
        "command": "<path-to-docket-cc>",
        "args": ["hook"],
        "env": { "DOCKET_CORE_URL": "http://<host>:<port>", "DOCKET_WORKER_ID": "<id>" }
      }]
    }]
  }
}
```

A sync failure (core unreachable, worker not registered) is swallowed to stderr, not stdout — a broken
connection reports nothing rather than injecting an error into every session's context.

## 8. Console

`docket-console` is a list→detail admin UI (secondary Board/kanban view also available), polling
every 5s — a pure HTTP client, no `docket-cc` involved. Item/comment body text renders as sanitized
markdown, including `![alt](url)` images — the URL must point to an already-hosted image; the
console has no upload/storage of its own. Besides browsing (state/tag/topic filters,
full-text search across title/body/comments), the detail view shows `requester`/`assignee`/`turn`
alongside state and can claim/submit/approve an item, edit its tags, reject/reopen it with a
required reason, and — for any item not yet `closed` — remove/merge/force-close/force-approve it
(§4's admin operations). Archive is available from any state (idempotent, no unarchive yet). Delete is too —
unlike every other action here, it requires typing the item's exact title before the button
enables, since it's the one operation that destroys tags/comments with no way back (§4's
`remove_item` vs `delete_item` note). Writes are attributed to a fixed `console` worker id;
multi-user identity is out of scope while docket stays single-owner. In production, `docket-core` itself serves the built console
at `/` (`DOCKET_CONSOLE_DIR`, default `console/dist`); the same API is available at that origin under
`/api/*`. For local dev: `cd console && npm install && npm run dev` (proxies to `127.0.0.1:8420` by
default; override via `.env`'s `VITE_DOCKET_CORE_URL`).

## 9. Current limitations

- **No authentication.** Anyone who can reach `docket-core`'s port can read and write everything.
  That now includes `DELETE /items/{id}`, which destroys an item, its tags, and its comments with
  no way to get them back — unlike `reject`/`reopen`/`archive`, every one of which is recoverable.
  Keep it off untrusted networks until M4.
- **No push/streaming.** Every read is a poll (`list_items`, `docket-console`'s 5s interval,
  `docket-cc hook`'s one-shot sync on session start) — nothing notifies a worker when new work lands.
- **No unarchive.** `archive_item` has no inverse yet — setting `archived_at` back to `null` isn't
  destructive, so this is a surface-area gap rather than a one-way door, and additive to fix later
  if the need shows up. (`reopen_item` covers the analogous gap for `state` — see §4 — so this
  limitation is about the archive axis specifically, not about closed items in general.)
- **No cap on reject/reopen cycles.** An item can bounce between `claimed` and `resolved`/`closed`
  indefinitely; there's no loop-detection or count limit. A repeatedly-bounced item just stays in
  the ordinary `claimed` bucket, which existing stall-detection already covers without new logic.

## 10. Where to go deeper

[architecture.md](architecture.md) (system boundaries, the four-layer split MCP/HTTP sits inside),
[glossary.md](glossary.md) (full vocabulary + the reasoning behind each term), [roadmap.md](roadmap.md)
(what's built vs. planned), [decisions/](decisions/) (the ADRs behind specific choices above, e.g. why
`claim` is exclusive, why there's no daemon yet).
