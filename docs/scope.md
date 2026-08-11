Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Scope

## In

- All four layers: `docket-core` (work-queue engine) · `docket-mcp` (MCP surface) · `docket-cc` (Claude Code adapter) · `docket-console` (admin UI)
- The worker/topic/item/claim domain model (core)
- Item lifecycle: `open → claimed → resolved → closed` + `resolution` ([architecture.md](architecture.md))
- Stall detection (distinguishing "no owner" from "no update")
- Every admin-console operation — especially "refine" (clean up an ambiguous request and re-queue it)
- `question` — a stateless, fail-immediately request type
- Single repo, fully public ([ADR-0005](decisions/ADR-0005-public-scope.md))

## Out

| Item | Rationale |
|---|---|
| File sync | git already solves this. The core only deals with references (`refs`) |
| Automatic work distribution (orchestration) | P-3; a human's manual assignment is the exception |
| Real-time chat | Explicitly given up in the differentiation hypothesis (an axis it's fine to lose on) |
| Multi-worker collaborative claims | Settled as single-claim only — exclusive claiming is part of the domain model's definition |
| Prompt-injection defense (in full) | Excluded from v1 coverage — deferred to L2~L3 ([coverage.md](coverage.md)) |
| Regulatory compliance, performance tuning | Settled as no hard constraints ([principles.md](principles.md)) |

## Later

| Item | Condition | Related doc |
|---|---|---|
| Multi-user (team-scale) | Revisit once the §12.1 auth approach is settled | [ADR-0006](decisions/ADR-0006-single-owner-later.md) |
| Other agent runtimes (e.g. `aims`) | The structure already supports adding just layer 3. Once real demand shows up | [architecture.md](architecture.md) extension points |
| Human workers (a person picking up items from a phone) | Possible with layer-4-only extension, no core changes. Once demand shows up | [architecture.md](architecture.md) |
| Standardizing non-repo topic namespaces | Once topics that aren't repos (org knowledge, machines, environments) actually start being used | [open-questions.md](open-questions.md) #8 |
