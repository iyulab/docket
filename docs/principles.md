Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Principles

## Philosophy

A Claude Code session is just one kind of worker. This sentence governs the whole design. Something that would have existed even without AI — a queue and a board for headless workers — is what core and console own; the AI-only surfaces (MCP, the Claude Code adapter) sit thinly on top of that.

## Principles

Next to each principle is the moment it starts costing something. A principle with no cost isn't a principle.

### P-1. Core doesn't know its consumers (non-negotiable)

No concept other than worker, topic, item, claim, body, stream, budget, tag, comment enters the core. The vocabulary mapping in [glossary.md](glossary.md) enforces this mechanically.

**When it gets expensive**: every time there's a temptation to drop a Claude-Code-specific feature (session resume, CLAUDE.md awareness) straight into the core, you have to keep paying the cost of pushing that concept up to the application layer and translating it instead.

### P-2. Files are not the source of truth

Only the core DB is authoritative; local files (`docket-cc`'s directory representation) are just a projection of it.

**When it gets expensive**: whenever a user wants to edit a file directly to change state — honoring this principle means every change has to go through the API, giving up one intuitive shortcut. Still undecided whether that trade-off is worth revisiting.

### P-3. Not an orchestrator, pull only

The center never auto-distributes work to workers. A human's manual intervention (the console's "force-assign") is not subject to this constraint.

**When it gets expensive**: unassigned items can pile up with no automatic distribution, and someone has to wait for a session to spin up and notice them.

## Quality-attribute priority

**Simplicity > reliability > scalability.**

- Scalability ranks lowest because P-1 already buys most of it structurally (layer separation + enforced vocabulary), without having to chase it separately.
- Reliability ranks below simplicity but not last: something like claim exclusivity is part of the domain model's own definition ([architecture.md](architecture.md) `claim`), not an implementation detail of reliability — so it's never sacrificed in the name of "keep it simple."
- Hard constraints (regulatory, performance, team, deadline): none. This is a single-person dogfooding tool at small scale, so no separate performance target is set. The deadline is handled by the stop-loss criteria in [goals.md](goals.md) instead.

## Non-goals

Full In/Out/Later breakdown is in [scope.md](scope.md). Here we only record *why* each non-goal is a non-goal.

- **Not a file-sync service** — git already solves this well for code and artifacts. No reason to reinvent it.
- **Not an orchestrator** — same rationale as P-3.
- **Not real-time chat** — the differentiation hypothesis in [goals.md](goals.md) explicitly gives up real-time-ness as an axis it's fine to lose on.
- **Not a multi-user collaboration tool** — the primary target is one person owning multiple machines. This one is Later, though ([ADR-0006](decisions/ADR-0006-single-owner-later.md)).
