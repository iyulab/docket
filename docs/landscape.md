Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Landscape

Items that haven't been investigated aren't filled in here. Only confirmed facts go in; everything else is pushed to a SPIKE.

## Current alternatives (confirmed)

| Alternative | Approach | Failure point |
|---|---|---|
| File-based draft issues | Another repo's Claude Code session reads the file directly. Crossing machines needs a git commit/push or copying/moving the file | No status tracking, no notifications, cross-machine delay |

## Similar products / frameworks

**`[SPIKE B-01]` done (2026-08-12).** Several tools exist; none confirmed to combine docket's specific pairing of (a) a persistent store items survive in without an owner and (b) cross-repo topic-prefix ownership behind a protocol-neutral core.

| Tool | Approach | Failure point (relative to docket) |
|---|---|---|
| [Swarm Protocol](https://github.com/phuryn/swarm-protocol) | MCP server; `claim_work` states "I'm taking this," periodic heartbeat signals liveness, "unblocks" handoff to the next agent | No confirmed durable store — state sync leans on messages/heartbeats between agents rather than a queryable DB a dead worker's item survives in |
| Hermes Kanban | SQLite-backed board; a dispatcher loop reclaims stale claims every ~60s across named agent profiles | Tied to one product's own agent-profile system, not a general core any kind of worker (human, script, other AI runtime) can pull from |
| [Code Conductor](https://github.com/ryanmac/code-conductor) | GitHub Issues as the queue (`conductor:task` label); an agent claims an issue, works in an isolated git worktree, opens a PR | Coordination substrate is GitHub Issues itself, not a dedicated state model; built around same-repo parallelism (worktrees), not cross-repo/cross-machine topic routing |
| Vibe Kanban | Kanban UI for managing AI coding agent tasks (Apache-2.0, community-maintained) | UI-first tool for watching/managing agent tasks, not a headless engine with an API other layers (mcp/cc/console) build on |

## Tentative differentiation hypothesis

- **Axis we win on**: status tracking / a single source of truth for state — the core owns something a kanban state had nowhere to live before. Confirmed as still differentiating against Swarm Protocol (heartbeat/message-based, no confirmed persistent store) and Code Conductor (GitHub Issues as substrate); Hermes Kanban does have a comparable SQLite-backed store but couples it to its own agent-profile system rather than a protocol-neutral core.
- **Axis we're fine losing on**: real-time-ness (batching notifications at turn boundaries is fine) + multi-worker collaboration (single-claim is sufficient).

Hypothesis holds after B-01; no re-validation forced. Re-open if a tool combining a protocol-neutral persistent core with cross-repo topic ownership turns up later.
