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
| `state` | `open → claimed → resolved → closed` — the workflow stage |
| `resolution` | Why an item closed: `done` (requester approval) / `duplicate` (merge) / `wontfix` (force-close) / `invalid` (remove) |
| `requester` / `assignee` / `turn` | `requester` is who the item is for, `assignee` is the current holder (was `owner`), `turn` says whose hand it's in right now — derived from `state`, not stored. See [ADR-0010](decisions/ADR-0010-item-from-to-turn.md) / [ADR-0011](decisions/ADR-0011-requester-assignee-naming.md) |
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
| `list_items` | `topic?`, `state?`, `assignee?`, `requester?`, `topic_scope?` | `GET /items?topic=&state=&assignee=&requester=&topic_scope=` | `topic_scope=<worker id>` is how a worker discovers its own queue (§5) — matches by topic jurisdiction, not who currently holds any given item. `assignee`/`requester` match the current assignee/requester exactly |
| `search_items` | `query?`, `tags[]?`, `tag_match?`, `topic?`, `state?` | `GET /items?q=&tag=&tag=&tag_match=&topic=&state=` | `query` full-text matches title+body+comments; `tag_match` is `any` (default) or `all` |
| `claim_item` | `item_id`, `worker_id` | `POST /items/{id}/claim {"worker_id"}` | `open → claimed`. Exclusive — loser gets a tool-level error, not a crash |
| `submit_item` | `item_id`, `worker_id` | `POST /items/{id}/submit {"worker_id"}` | `claimed → resolved`. Only the current assignee may submit |
| `approve_item` | `item_id` | `POST /items/{id}/approve` | `resolved → closed`, `resolution=done`. The requester's sign-off |
| `add_tags` / `remove_tags` | `item_id`, `tags[]` | `POST`/`DELETE /items/{id}/tags {"tags"}` | Idempotent both ways |
| `list_tags` | `topic?` | `GET /tags?topic=` | **Call before tagging** to reuse existing vocabulary instead of inventing a synonym. Returns `{tag, count}[]`, most-used first |
| `add_comment` | `item_id`, `body`, `author?` | `POST /items/{id}/comments {"author","body"}` | `author` defaults to `"unknown"` if omitted |
| `list_comments` | `item_id` | `GET /items/{id}/comments` | Chronological, append-only |

`GET /items/{id}` also exists at the HTTP level (fetch one item by id) but has no MCP tool
equivalent yet — reach it directly if you're a plain HTTP client, or `list_items`/`search_items` and
filter if you're going through MCP.

`PATCH /items/{id} {"requester": "…"}` sets `requester` on an item that already exists — the only
field this covers so far, and the only way to give an item a requester after creation (`requester` is
normally set once at creation, ADR-0010/ADR-0011). Meant for backfilling items filed before a
requester identity was available, not routine editing — there's no MCP tool for it (same admin-only
reasoning as the three close operations below) and no way yet to edit `title`/`body`/`topic` after
creation. State-independent (works on a closed item too — it corrects metadata, not a workflow
transition). Rejects a blank `requester` with `400`, a missing item with `404`.

Three more HTTP-only admin operations close an item early, bypassing the normal
`claimed → resolved → closed` path — they're console/admin actions (`docket-console` exposes them as
buttons), not worker actions, so there's no MCP tool for them. All three are assignee-agnostic and valid
from any state except `closed` (unlike `approve`, they don't require reaching `resolved` first):

| HTTP | resolution | Meaning |
|---|---|---|
| `POST /items/{id}/remove` | `invalid` | The item was a mistake — never should have been filed |
| `POST /items/{id}/merge` | `duplicate` | Consolidated into another item |
| `POST /items/{id}/force-close` | `wontfix` | No longer relevant, closed without being done |

An `Item` looks like:

```json
{
  "id": "…", "topic": "iyulab/docket", "title": "…", "body": null,
  "state": "open", "resolution": null, "requester": null, "assignee": null, "turn": null,
  "tags": [], "created_at": 1734000000000, "updated_at": 1734000000000
}
```

`requester`/`assignee`/`turn` are the two-party handoff — `requester` is who this item is being
worked for, `assignee` is the current holder (was `owner`), `turn` is derived from `state` and tells
you whose hand it's in right now (`"assignee"` while claimed, `"requester"` while resolved and
awaiting approval, `null` when open or closed). See
[ADR-0010](decisions/ADR-0010-item-from-to-turn.md) /
[ADR-0011](decisions/ADR-0011-requester-assignee-naming.md).

Errors are `{"error": "<message>"}` with `404` (not found), `409` (state conflict — e.g. `"cannot
claim: item is claimed"`), or `500` (server-side failure). A `claim`/`submit`/`approve`/`remove`/
`merge`/`force-close` call that loses a race or targets the wrong state always comes back `409`,
never `500` — that's the signal to re-`list_items` and try something else rather than treat it as a
bug.

**Reads never 404 on a non-matching or unregistered reference — writes do.** `list_items`/
`search_items`/`list_comments`/`list_tags` answer any filter that matches nothing (an unknown
`topic`, `assignee`, `requester`, `topic_scope` worker id, or `item_id`) with an empty result, the
same way a database query does — there is no "does this reference exist" check on a read path.
`create_item`/`claim_item`/`submit_item`/`approve_item`/`add_comment`/`add_tags`/`remove_tags`/the
three admin close operations all target one specific item (or, for `topic_scope`, look up one
specific worker as a side effect of a *write*'s validation) and 404 when it doesn't exist — a mutate
call has nothing sensible to do with "no such reference" other than fail. Rely on this instead of
treating an empty list as ambiguous: it always means "no matches", never "the thing you filtered by
doesn't exist" — there's nothing else it could mean, since reads don't look that up in the first
place.

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
every 5s — a pure HTTP client, no `docket-cc` involved. Besides browsing (state/tag/topic filters,
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
  Keep it off untrusted networks until M4.
- **No push/streaming.** Every read is a poll (`list_items`, `docket-console`'s 5s interval,
  `docket-cc hook`'s one-shot sync on session start) — nothing notifies a worker when new work lands.
- **No reopen.** Once an item reaches `closed` (by any of `approve`/`remove`/`merge`/`force-close`),
  there's no API to move it back — a mistaken close is permanent.

## 10. Where to go deeper

[architecture.md](architecture.md) (system boundaries, the four-layer split MCP/HTTP sits inside),
[glossary.md](glossary.md) (full vocabulary + the reasoning behind each term), [roadmap.md](roadmap.md)
(what's built vs. planned), [decisions/](decisions/) (the ADRs behind specific choices above, e.g. why
`claim` is exclusive, why there's no daemon yet).
