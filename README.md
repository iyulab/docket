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

**M1 done, M2 in progress, M3 started** ([roadmap.md](docs/roadmap.md)). `docket-core` (domain model, SQLite-backed store, HTTP API), `docket-mcp` (exposes the same operations as MCP tools), and `docket-cc` (file projection + a `SessionStart` hook) exist and are covered by tests. `docket-console` exists as a read-only kanban board — see "As a console" below.

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
curl "localhost:8420/items?owned_by=w1&state=open"

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

#### Installing docket-mcp (without building from source)

```bash
curl -fsSL https://raw.githubusercontent.com/iyulab/docket/main/scripts/install.sh | sh
```

```powershell
irm https://raw.githubusercontent.com/iyulab/docket/main/scripts/install.ps1 | iex
```

This installs a small launcher as `docket-mcp` that checks GitHub Releases for updates on every
run and caches the latest build locally — point your MCP client's `command` at it the same way as
above, no `cargo run` needed. Set `DOCKET_INSTALL_DIR` before running the script to install
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

### As a console

`docket-console` is a read-only kanban board: every item, grouped into `open` / `claimed` /
`resolved` / `closed` columns, polling `docket-core` every 5 seconds. It's a pure client of the
core HTTP API — no writes, no `docket-cc` involved.

```bash
cd console
npm install
npm run dev
```

By default it proxies to `docket-core` at `http://127.0.0.1:8420`. Point it elsewhere by copying
`.env.example` to `.env` and setting `VITE_DOCKET_CORE_URL`. This is the first slice of M3 — see
[roadmap.md](docs/roadmap.md#m3--console) for what's still ahead (write operations, stall
detection, refine).

## Docs

| Doc | Contents |
|---|---|
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
