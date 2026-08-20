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
| `resolution` | Why an item closed: `done` (requester approval) / `duplicate` (merge) / `wontfix` (force-close) / `invalid` (remove) |
| `open` | `true` while `state != closed`, else `false` — derived from `state`, not stored, same treatment as `turn`. See [ADR-0012](decisions/ADR-0012-item-reject-reopen-transitions.md) |
| `requester` / `assignee` / `turn` | `requester` is who the item is for, `assignee` is the current holder (was `owner`), `turn` says whose hand it's in right now — derived from `state`, not stored. See [ADR-0010](decisions/ADR-0010-item-from-to-turn.md) / [ADR-0011](decisions/ADR-0011-requester-assignee-naming.md) |
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

| Tool | Params | HTTP | Notes |
|---|---|---|---|
| `register_worker` | `id`, `topics[]` | `POST /workers` | Call once per session. `topics` are prefixes — see §5 |
| `create_item` | `topic`, `title`, `body?`, `tags[]?`, `requester?` | `POST /items` | **Call `search_items` first** to avoid filing a duplicate. `requester` is who this item is being worked for — optional (see [ADR-0010](decisions/ADR-0010-item-from-to-turn.md) / [ADR-0011](decisions/ADR-0011-requester-assignee-naming.md)) |
| `list_items` | `topic?`, `state?`, `assignee?`, `requester?`, `topic_scope?`, `archived?`, `limit?`, `offset?` | `GET /items?topic=&state=&assignee=&requester=&topic_scope=&archived=&limit=&offset=` | `topic_scope=<worker id>` is how a worker discovers its own queue (§5) — matches by topic jurisdiction, not who currently holds any given item. `assignee`/`requester` match the current assignee/requester exactly. `archived` defaults to `false` (today's behavior); `true` browses only the archive — see §4's archiving note. **Tool result is `{items, total}`, not a bare array** — `limit` defaults to 50, capped at 200; `total` is the row count before that cap, so a `total` above `items.length` means page further with `offset` ([ADR-0014](decisions/ADR-0014-list-search-pagination-and-list-topics.md)). Over HTTP the body stays a bare `Item[]`; `total` comes back as the `X-Total-Count` header instead |
| `search_items` | `query?`, `tags[]?`, `tag_match?`, `topic?`, `state?`, `archived?`, `limit?`, `offset?` | `GET /items?q=&tag=&tag=&tag_match=&topic=&state=&archived=&limit=&offset=` | `query` full-text matches title+body+comments — matched word-by-word (each word independently, not as one exact adjacent phrase), so word order doesn't matter and a query word also prefix-matches a token carrying a suffix it doesn't have (e.g. a stemmed or CJK-particle-suffixed form). `tag_match` is `any` (default) or `all`; `archived`/`limit`/`offset`/response shape same as `list_items` above |
| `claim_item` | `item_id`, `worker_id` | `POST /items/{id}/claim {"worker_id"}` | `open → claimed`. Exclusive — loser gets a tool-level error, not a crash |
| `submit_item` | `item_id`, `worker_id` | `POST /items/{id}/submit {"worker_id"}` | `claimed → resolved`. Only the current assignee may submit |
| `approve_item` | `item_id`, `author?` | `POST /items/{id}/approve {"author"}` | `resolved → closed`, `resolution=done`. The requester's sign-off. `author` defaults to `"unknown"` if omitted |
| `reject_item` | `item_id`, `reason`, `author?` | `POST /items/{id}/reject {"reason","author"}` | `resolved → claimed`. The requester sending it back for more work — **not** done yet. `reason` is required (recorded as a comment, atomically with the state change) |
| `reopen_item` | `item_id`, `reason`, `author?` | `POST /items/{id}/reopen {"reason","author"}` | `closed → claimed` (or `→ open`, if the item was closed before anyone ever claimed it), clears `resolution` back to `null`. For a close that turns out to have been premature or mistaken. `reason` is required, same as `reject_item` |
| `archive_item` | `item_id` | `POST /items/{id}/archive` | Hides the item from default `list_items`/`search_items` results (still fully queryable with `archived: true`). Idempotent, no data lost, works from any `state`. No `unarchive` yet |
| `add_tags` / `remove_tags` | `item_id`, `tags[]` | `POST`/`DELETE /items/{id}/tags {"tags"}` | Idempotent both ways |
| `list_tags` | `topic?` | `GET /tags?topic=` | **Call before tagging** to reuse existing vocabulary instead of inventing a synonym. Returns `{tag, count}[]`, most-used first |
| `list_topics` | — | `GET /topics` | **Call before `list_items(topic=…)`** to discover topic names instead of guessing/enumerating them. Returns `{topic, count}[]`, most-populated first; excludes topics whose items are all archived, same as an all-archived `list_items` query would |
| `add_comment` | `item_id`, `body`, `author?` | `POST /items/{id}/comments {"author","body"}` | `author` defaults to `"unknown"` if omitted |
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

`GET /items/{id}` and `GET /workers/{id}` also exist at the HTTP level (fetch one item/worker by id)
but have no MCP tool equivalent yet — reach them directly if you're a plain HTTP client, or
`list_items`/`search_items` and filter if you're going through MCP. `GET /workers/{id}` is the only
way to positively confirm a worker is registered — see the read/write not-found note below.

`PATCH /items/{id} {"requester": "…"}` sets `requester` on an item that already exists — the only
field this covers so far, and the only way to give an item a requester after creation (`requester` is
normally set once at creation, ADR-0010/ADR-0011). Meant for backfilling items filed before a
requester identity was available, not routine editing — there's no MCP tool for it (same admin-only
reasoning as the admin operations below) and no way yet to edit `title`/`body`/`topic` after
creation. State-independent (works on a closed item too — it corrects metadata, not a workflow
transition). Rejects a blank `requester` with `400`, a missing item with `404`.

Three more HTTP-only operations close an item early, bypassing the normal
`claimed → resolved → closed` path — they're console/admin actions (`docket-console` exposes them as
buttons), not worker actions, so there's no MCP tool for them. All three are assignee-agnostic and valid
from any state except `closed` (unlike `approve`, they don't require reaching `resolved` first):

| HTTP | resolution | Meaning |
|---|---|---|
| `POST /items/{id}/remove {"author"}` | `invalid` | The item was a mistake — never should have been filed |
| `POST /items/{id}/merge {"author"}` | `duplicate` | Consolidated into another item |
| `POST /items/{id}/force-close {"author"}` | `wontfix` | No longer relevant, closed without being done |

All three take an optional `author`, recorded as a comment alongside the close, exactly like
`approve_item` above — it defaults to `"unknown"` if omitted, and the request body itself may be
omitted entirely (a bodiless `POST` is accepted and takes the same default).

> **`remove_item` is not `delete_item` — do not confuse the two.**
>
> - **`POST /items/{id}/remove`** (table above) *closes* the item: `state → closed`,
>   `resolution → invalid`. The item, its tags, and its comments all still exist and are still
>   queryable — this is how you mark "this should never have been filed" while keeping a
>   permanent, traceable record of that fact.
> - **`DELETE /items/{id}`** (`delete_item`, HTTP-only, no MCP tool, no `author`/`reason` params —
>   there is no item left afterward to attach either to) *destroys* the item outright: the row,
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
`resolved` (awaiting approval), `null` only while `closed` (done — nobody's turn). See
[ADR-0010](decisions/ADR-0010-item-from-to-turn.md) /
[ADR-0011](decisions/ADR-0011-requester-assignee-naming.md).

`open` is `state != closed`, computed the same way as `turn` — never stored, so it can never drift
out of sync with `state`. `archived_at` is `null` unless the item was archived; it's independent of
`state` (an item in any workflow state can be archived) and only affects whether default
`list_items`/`search_items` calls surface the item.

Errors are `{"error": "<message>"}` with `404` (not found), `409` (state conflict — e.g. `"cannot
claim: item is claimed"`), or `500` (server-side failure). A `claim`/`submit`/`approve`/`reject`/
`reopen`/`remove`/`merge`/`force-close` call that loses a race or targets the wrong state always
comes back `409`, never `500` — that's the signal to re-`list_items` and try something else rather
than treat it as a bug. `archive_item` and `delete_item` are state-unrestricted (valid from any
state) so this doesn't apply to either.

**A *list/search* filter never 404s on a non-matching or unregistered reference — a call that
targets one specific known resource by id does.** `list_items`/`search_items`/`list_comments`/
`list_tags` answer any filter that matches nothing (an unknown `topic`, `assignee`, `requester`,
`topic_scope` worker id, or `item_id`) with an empty result, the same way a database query does —
there is no "does this reference exist" check on a filter. `GET /items/{id}` and
`GET /workers/{id}`, and every mutate call (`create_item`/`claim_item`/`submit_item`/`approve_item`/
`reject_item`/`reopen_item`/`archive_item`/`add_comment`/`add_tags`/`remove_tags`/`delete_item`/the
three admin close operations), target one specific item or worker by id and 404 when it doesn't
exist — fetching or acting on one named thing has nothing sensible to do with "no such reference"
other than fail. Rely on this instead of treating an empty
list as ambiguous: it always means "no matches", never "the thing you filtered by doesn't exist" —
there's nothing else it could mean, since a filter doesn't look that up in the first place.
`list_items(topic_scope=<id>)` in particular can't tell you whether `<id>` is a registered worker —
it treats "unregistered" the same as "registered, no matching topics" (both: empty result) — call
`GET /workers/{id}` directly if you need to know which.

## 5. The worker loop

This is the pattern an agent repeats:

1. **Once per session**: `register_worker(id, topics)` — `topics` are prefixes (`"iyulab"` owns every
   topic starting `iyulab/…`, exact match or `/`-delimited prefix; see `topic_matches` in
   [glossary.md](glossary.md)).
2. **Discover work**: `list_items(topic_scope=<your id>, state="open")`.
3. **Before filing something new**: `search_items(query=…)` — check it doesn't already exist.
4. **Take an item**: `claim_item(item_id, worker_id)`. If it 409s, someone else got there first — go
   back to step 2.
5. Do the actual work.
6. **Finish**: `submit_item(item_id, worker_id)` — moves it to `resolved`, waiting on the requester.
7. The requester (whoever wanted the item done — may be a different worker, or a human via
   `docket-console`) calls `approve_item(item_id)` once satisfied, closing it.

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
alongside state and can claim/submit/approve an item and
edit its tags, and — for any item not yet `closed` — remove/merge/force-close it (§4's admin
operations). Writes are attributed to a fixed `console` worker id; multi-user identity is out of
scope while docket stays single-owner. In production, `docket-core` itself serves the built console
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
