Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Roadmap

The further-out milestones are more abstract; the nearer ones are more concrete.

## M1 — Core (first slice)

**Scope**
- Language: Rust
- Minimal core API set: register worker, create item (file), claim, submit (→resolved), requester approval (→closed, resolution=done)
- Storage: SQLite
- Interface: HTTP API only. No mcp/cc/console. A human pretends to be a worker via curl.

**Completion criteria**: from two terminals, act as two different "workers" using curl. Worker A creates an item in front of topic X → Worker B registers as owning X and discovers it via `list` → claims it → submits it → Worker A approves it (closes it). SQLite records the state transitions exactly. If two workers try to claim the same item at the same time, only one succeeds (verifies claim exclusivity).

**Status**: met — see `docket-core`'s test suite and the README "Running it" walkthrough.

Rationale: [ADR-0001](decisions/ADR-0001-work-queue-model.md), [ADR-0007](decisions/ADR-0007-language-runtime.md).

## M2 — Proof of existence

`docket-mcp` + `docket-cc`. Two sessions actually hand items back and forth to manually complete [vision.md](vision.md) S1~S6. Once this works, the product exists.

Hook-driven active notifications aren't required at this stage — it must be possible to complete the loop using only manual MCP calls (overlaps with the A-1 validation in [goals.md](goals.md)).

**Status**: `docket-mcp` done (register/create/list/claim/submit/approve as MCP tools, an HTTP client of `docket-core` — see README "As an MCP server"). `docket-cc` not started — file representation location settled ([ADR-0008](decisions/ADR-0008-file-representation-location.md)), so scaffolding can begin.

## M3 — Console

The board, stall detection, the full set of admin operations. Especially "refine" (see [vision.md](vision.md) S5) — this is the console's reason for existing. This is also where adoption-rate/burndown measurement actually starts ([goals.md](goals.md)).

## M4 — Safeguards and multiple machines

The full budget mechanism, auth, cross-machine routing, distribution channels.

## Off the roadmap

Multi-user collaboration doesn't appear in any milestone of this roadmap — it's Later ([ADR-0006](decisions/ADR-0006-single-owner-later.md)).
