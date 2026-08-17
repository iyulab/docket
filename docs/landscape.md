Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Landscape

Items that haven't been investigated aren't filled in here. Only confirmed facts go in; everything else is pushed to a SPIKE.

## Current alternatives (confirmed)

| Alternative | Approach | Failure point |
|---|---|---|
| File-based draft issues | Another repo's Claude Code session reads the file directly. Crossing machines needs a git commit/push or copying/moving the file | No status tracking, no notifications, cross-machine delay |

## Similar products / frameworks

Several tools exist; none confirmed to combine docket's specific pairing of (a) not owning or spawning the worker's process and (b) cross-repo topic-prefix ownership behind a protocol-neutral core.

| Tool | Approach | Failure point (relative to docket) |
|---|---|---|
| [Swarm Protocol](https://github.com/phuryn/swarm-protocol) | MCP server backed by PostgreSQL; `claim_work` states "I'm taking this," periodic heartbeat signals liveness, "unblocks" handoff to the next agent; explicitly targets multiple people each running their own agent | Spawns/owns the agent process itself (session orchestrator), and the coordination surface is MCP-only — no protocol-neutral core other runtimes can sit behind |
| Hermes Kanban | SQLite-backed board; a dispatcher loop reclaims stale claims every ~60s across named agent profiles | Tied to one product's own agent-profile system, not a general core any kind of worker (human, script, other AI runtime) can pull from |
| [Code Conductor](https://github.com/ryanmac/code-conductor) | GitHub Issues as the queue (`conductor:task` label); an agent claims an issue, works in an isolated git worktree, opens a PR | Coordination substrate is GitHub Issues itself, not a dedicated state model; built around same-repo parallelism (worktrees), not cross-repo/cross-machine topic routing |
| Vibe Kanban | Kanban UI for managing AI coding agent tasks (Apache-2.0, community-maintained) | UI-first tool for watching/managing agent tasks, not a headless engine with an API other layers (mcp/cc/console) build on |

## Tentative differentiation hypothesis

A single source of truth for state, on its own, is no longer the axis to lead with — Swarm Protocol turns out to be backed by PostgreSQL, so "some tools have no durable store" doesn't hold as a general claim. What holds up against every tool in the table above:

- **Axis we win on — doesn't own the worker's process.** Swarm Protocol, Hermes Kanban, and Code Conductor all spawn, attach to, or otherwise take responsibility for the agent process itself; docket only ever holds a durable item a worker chooses to pull, and has no opinion on how that worker's process came to exist.
- **Axis we win on — protocol-neutral core.** Every tool above exposes its coordination surface as an MCP tool (or a GitHub Issues label convention) with no separable core underneath. docket's core is an HTTP API with no MCP dependency; `docket-mcp` is one thin, replaceable surface over it, so another agent runtime can sit behind the same core without going through MCP at all.
- **Axis we win on — cross-repo topic ownership for one owner, many machines.** None of the tools above let a single worker declare ownership of a path prefix that spans multiple repositories, the way docket's `topic` prefix matching does.
- **No longer a differentiator — stall detection.** Swarm Protocol's heartbeat and Hermes Kanban's reclaim loop both already solve "is this claim still alive," so once docket ships this, it will be closing a parity gap, not opening a lead.
- **Axis we're fine losing on**: real-time-ness (a worker checks in on its own schedule — see [vision.md](vision.md)) and multi-worker collaboration (single-claim is sufficient).

Hypothesis holds against the tools above; no re-validation forced. Re-open if a tool combining process-non-ownership, a protocol-neutral core, and cross-repo topic ownership turns up later.
