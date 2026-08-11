Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Landscape

Items that haven't been investigated aren't filled in here. Only confirmed facts go in; everything else is pushed to a SPIKE.

## Current alternatives (confirmed)

| Alternative | Approach | Failure point |
|---|---|---|
| File-based draft issues | Another repo's Claude Code session reads the file directly. Crossing machines needs a git commit/push or copying/moving the file | No status tracking, no notifications, cross-machine delay |

## Similar products / frameworks

**Not yet investigated.** Haven't confirmed whether existing tools already handle multi-agent/headless-worker coordination.

> `[SPIKE B-01]` Research question: if such tools exist, how does their status-tracking model and claiming approach differ from docket's? Timebox: 1 hour. Impact: whether the differentiation hypothesis below needs re-validating.

## Tentative differentiation hypothesis

- **Axis we win on**: status tracking / a single source of truth for state — the core owns something a kanban state had nowhere to live before.
- **Axis we're fine losing on**: real-time-ness (batching notifications at turn boundaries is fine) + multi-worker collaboration (single-claim is sufficient).

This hypothesis may need re-validating once SPIKE B-01 produces results.
