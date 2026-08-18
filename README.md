# docket

[![CI](https://github.com/iyulab/docket/actions/workflows/ci.yml/badge.svg)](https://github.com/iyulab/docket/actions/workflows/ci.yml)

A work-queue service for headless workers. A Claude Code session is just one kind of worker.

## Who it's for

Developers who keep multiple Claude Code (or similar headless) sessions running across several machines at once, where the sessions work on different repositories or on components that depend on each other. The core engine isn't tied to Claude Code, so other projects can reuse it for headless-worker coordination.

## What it isn't

- Not a file-sync service — artifact sharing is already git's job.
- Not an orchestrator — the center never auto-distributes work to workers. It's a pull model where workers pick up their own work (a human's manual intervention is the exception).
- Not real-time chat.
- Not a multi-user collaboration tool — the primary target is one person owning multiple machines (multi-user is Later, see [ADR-0006](docs/decisions/ADR-0006-single-owner-later.md)).

## Current status

**M1 done, M2 in progress, M3 started** ([roadmap.md](docs/roadmap.md)). `docket-core` (domain model, SQLite-backed store, HTTP API, tags, comments, full-text search over both), `docket-mcp` (exposes the same operations as MCP tools), and `docket-cc` (file projection, a `SessionStart` hook, and local topic derivation) exist and are covered by tests. `docket-console` exists — list→detail view with filters/search/sort plus claim/submit/approve, admin close (remove/merge/force-close), and tag editing (a kanban board is available as a secondary view) — see "As a console" below.

**Operating docket** (as an MCP-calling agent or a plain HTTP client — register, file, discover, claim, complete work): [docs/usage.md](docs/usage.md) is the single complete reference. The rest of this README is a shorter, example-driven walkthrough of the same ground.

## Running it

```
cargo run -p docket-core
```

Binds to `127.0.0.1:8420` by default — no auth exists yet, so this keeps the API off the network until M4 adds it. Override with `DOCKET_BIND` / `DOCKET_PORT`. Opens/creates a SQLite file at `docket.db` in the working directory (override with `DOCKET_DB_PATH`). Then, acting as two workers from two terminals:

```
JSON='-H Content-Type:application/json'

# create an item in front of a topic
curl -X POST localhost:8420/items $JSON \
  -d '{"topic":"iyulab/docket","title":"fix the thing"}'

# register as owning that topic, then discover the item via list
curl -X POST localhost:8420/workers $JSON -d '{"id":"w1","topics":["iyulab"]}'
curl "localhost:8420/items?topic_scope=w1&state=open"

# claim it, submit it, and have the requester approve it
curl -X POST localhost:8420/items/<id>/claim  $JSON -d '{"worker_id":"w1"}'
curl -X POST localhost:8420/items/<id>/submit $JSON -d '{"worker_id":"w1"}'
curl -X POST localhost:8420/items/<id>/approve
```

If two workers race to claim the same item, exactly one gets `200`; the other gets `409`.

### As an MCP server

`docket-mcp` exposes `register_worker`/`create_item`/`list_items`/`claim_item`/`submit_item`/`approve_item` as MCP tools, talking to `docket-core` over the same HTTP API (`DOCKET_CORE_URL`, default `http://127.0.0.1:8420` — it never links `docket-core` as a library, see [architecture.md](docs/architecture.md)). Add it to an MCP client's config as a stdio server:

```json
{
  "mcpServers": {
    "docket": {
      "command": "cargo",
      "args": ["run", "-p", "docket-mcp"],
      "env": { "DOCKET_CORE_URL": "http://127.0.0.1:8420" }
    }
  }
}
```

`docket-core` must already be running (see above). This is the manual-MCP-calls loop M2's completion criteria describes ([roadmap.md](docs/roadmap.md#m2--proof-of-existence)) — no hooks or automatic notifications yet, so a session has to be told to call these tools.

#### Installing docket-mcp and docket-cc (without building from source)

```bash
curl -fsSL https://raw.githubusercontent.com/iyulab/docket/main/scripts/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/iyulab/docket/main/scripts/install.ps1 | iex
```

This installs small launchers as `docket-mcp` and `docket-cc` that check GitHub Releases for
updates on every run and cache the latest build locally — point your MCP client's `command` at
`docket-mcp` and your Claude Code hook's `command` at `docket-cc` the same way as the examples
above and below, no `cargo run` needed. Set `DOCKET_INSTALL_DIR` before running the script to install
somewhere other than the default.

### As a local file projection

`docket-cc` (no arguments) projects every item in a worker's owned topics onto a local `.md` file (frontmatter + the item's `body` as markdown), so a session can read its work as files instead of MCP calls. The worker must already be registered (see above):

```
DOCKET_CORE_URL=http://127.0.0.1:8420 \
DOCKET_WORKER_ID=w1 \
DOCKET_CC_ROOT=~/.docket \
cargo run -p docket-cc
```

`DOCKET_CC_ROOT` defaults to a platform user-data directory ([ADR-0008](docs/decisions/ADR-0008-file-representation-location.md) — deliberately outside any repo). Layout mirrors the topic path: an item in `iyulab/docket` lands at `<root>/iyulab/docket/<item-id>.md`. This is one-shot and write-only for now — run it again to pick up changes; it doesn't yet remove a projection for an item that closed or left the worker's owned topics.

### As a Claude Code hook

`docket-cc hook` runs the same projection, then prints a plain-text summary of currently open items (nothing, if there are none) — meant for a `SessionStart` hook, whose stdout gets injected into the session's context automatically. Wire it into `.claude/settings.json`:

See [Installing docket-mcp and docket-cc](#installing-docket-mcp-and-docket-cc-without-building-from-source) above for how to get the `docket-cc` binary this points at.

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup",
        "hooks": [
          {
            "type": "command",
            "command": "docket-cc",
            "args": ["hook"],
            "env": {
              "DOCKET_CORE_URL": "http://127.0.0.1:8420",
              "DOCKET_WORKER_ID": "w1"
            }
          }
        ]
      }
    ]
  }
}
```

A sync failure inside `hook` (core unreachable, worker not registered) is swallowed — reported to stderr, not stdout — so a broken connection reports nothing rather than injecting an error into every session's context. There's no daemon behind this yet; each invocation does its own one-shot sync, and whether a persistent daemon is actually needed is still open (see [architecture.md](docs/architecture.md) for `docket-cc`'s full intended shape).

### As a topic-deriving helper

`docket-cc topic` prints the `topic` the current directory belongs to, without needing `DOCKET_WORKER_ID`/`DOCKET_CORE_URL` set — it never talks to `docket-core`, only the local filesystem:

```
cd path/to/some/repo && docket-cc topic
# iyulab/some-repo
```

The topic comes from the nearest `.git` above the current directory — its `origin` remote's `org/repo`. The whole repository is one topic by default, however many packages live inside it (a package inside a monorepo resolves to the same topic as the repository root). A submodule's own `.git` stops the walk at the submodule, so it resolves to its own remote rather than the umbrella repository's — and a `git worktree` resolves through to the same remote as the repository it was created from. Drop a `.docket/topic` file (one line, the topic to use) in any ancestor directory to override the derivation entirely — useful when there's no remote yet, or to opt a specific directory into a finer-grained topic than the repo-level default.

**`git worktree` and `claim` are orthogonal, not substitutes.** Running several Claude Code sessions in parallel usually means several `git worktree` checkouts of the same repository — but a worktree only isolates the working tree on disk; it says nothing about which session owns which item. Every worktree of the same repository resolves to the same topic (see above), so nothing about worktree isolation prevents two sessions from claiming — and safely racing for — items in that topic; `claim` is what makes exactly one of them win. Conversely, `claim` doesn't isolate a session's filesystem changes from another session's — that's what the worktree is for. Use both together: the worktree keeps two sessions from stepping on each other's files, `claim` keeps them from stepping on each other's work.

### As a console

`docket-console` is a list view over every item (a kanban board grouped into `open` / `claimed` /
`resolved` / `closed` columns is available as a secondary view), polling `docket-core` every 5
seconds. It's a pure client of the core HTTP API — no `docket-cc` involved. The list shows
`from`/`to`/`turn` ([ADR-0010](docs/decisions/ADR-0010-item-from-to-turn.md)) as columns, and
supports filtering by state/tag/topic, full-text search across title/body/comments, and a
topic-level from/to perspective toggle (a separate, topic-to-topic axis — see
[glossary.md](docs/glossary.md) — kept for items still carrying a legacy `found-in:` tag). Selecting
an item navigates to a dedicated detail page (rendered from markdown) rather than opening a side
panel, and supports the core write operations — claim, submit, approve, and tag add/remove —
attributed to a fixed `console` identity, since multi-user identity is Later
([ADR-0006](docs/decisions/ADR-0006-single-owner-later.md)). It also exposes the three admin close
operations (remove/merge/force-close, valid from any non-closed state, assignee-agnostic) that set
`resolution` to `invalid`/`duplicate`/`wontfix` — see [usage.md](docs/usage.md#8-console).

```bash
cd console
npm install
npm run dev
```

By default it proxies to `docket-core` at `http://127.0.0.1:8420`. Point it elsewhere by copying
`.env.example` to `.env` and setting `VITE_DOCKET_CORE_URL`. See
[roadmap.md](docs/roadmap.md#m3--console) for what's still ahead (stall detection, refine).

In production, `docket-core` itself serves the built console — no separate server needed. Run
`npm run build` in `console/`, then point `docket-core` at the output with `DOCKET_CONSOLE_DIR`
(defaults to `console/dist`, relative to `docket-core`'s working directory). It's served at `/`,
with client-side routes falling back to `index.html`; the API it talks to is available at the
same origin under `/api/*` (an alias for the same routes documented above). Only `/api/*` is
guaranteed a JSON error on an unmatched path — the root namespace falls back to the SPA's
`index.html` for anything it doesn't recognize, since that's what client-side routing needs.

## Development

`scripts/verify.sh` (`scripts/verify.ps1` on Windows) runs the same fmt-check → clippy → build → test sequence as `.github/workflows/ci.yml`, in the same order, so a failure shows up locally before it shows up in CI:

```bash
scripts/verify.sh
```

When developing `docket-mcp`/`docket-cc` themselves, set `DOCKET_LAUNCHER_LOCAL_BIN` to a locally-built binary's path to make either launcher exec it directly — no GitHub Releases check, no cache, no checksum verification:

```bash
cargo build -p docket-cc
DOCKET_LAUNCHER_LOCAL_BIN=target/debug/docket-cc cargo run -p docket-cc-launcher -- hook
```

## Docs

| Doc | Contents |
|---|---|
| [usage.md](docs/usage.md) | **Start here to operate docket** — MCP tools, HTTP API, the worker loop, topic derivation |
| [vision.md](docs/vision.md) | Problem · users · scenarios |
| [principles.md](docs/principles.md) | Philosophy · principles · non-goals |
| [scope.md](docs/scope.md) | In / Out / Later |
| [goals.md](docs/goals.md) | North star · goal tree · metrics · stop-loss criteria |
| [landscape.md](docs/landscape.md) | Landscape of alternatives (mostly unsurveyed) |
| [architecture.md](docs/architecture.md) | System boundaries · domain model · Type-1 decisions |
| [roadmap.md](docs/roadmap.md) | Milestones and the first slice |
| [glossary.md](docs/glossary.md) | Core vocabulary mapping |
| [decisions/](docs/decisions/) | ADRs — one file per Type-1 decision |

A data strategy doc isn't included since it isn't core to this project — docket deals with operational state (workers/items/claims), not training data.
