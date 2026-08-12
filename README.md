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

**M1 done, M2 in progress** ([roadmap.md](docs/roadmap.md)). `docket-core` (domain model, SQLite-backed store, HTTP API) and `docket-mcp` (exposes the same operations as MCP tools) exist and are covered by tests. No `docket-cc`/`docket-console` yet — without `docket-cc`'s hooks, an AI worker has to be told to call the MCP tools explicitly rather than being notified automatically.

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

Rules future sessions follow are in [AGENTS.md](AGENTS.md).
