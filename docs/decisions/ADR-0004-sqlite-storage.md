Status: v0 alignment snapshot | 2026-08-11 | settled

# ADR-0004: Storage engine — single-instance SQLite

## Context

The core's storage engine is a hard-to-reverse choice about where the source-of-truth data lives ([principles.md](../principles.md) P-2 — files aren't the source of truth, the DB is).

## Options considered and trade-offs

- **Distributed DB / client-server RDBMS**: prepares for multi-machine, multi-user scale, but is over-engineered relative to the currently settled hard constraints (none, [principles.md](../principles.md)) and the top-priority quality attribute (simplicity).
- **Single-instance SQLite**: low installation friction, and matches the primary target scale — single owner, multiple machines ([scope.md](../scope.md)) — exactly.

## Decision

Start with a single-instance SQLite.

## Consequences

**Gained**: installation simplicity (the target install experience — spin up one server and you're done, see [ADR-0007](ADR-0007-language-runtime.md)). Matches the top-priority quality attribute.

**Given up**: multiple servers / horizontal scaling isn't possible right away with this decision. If multi-user ([ADR-0006](ADR-0006-single-owner-later.md)) becomes real, this decision needs revisiting alongside it.

## Re-open trigger

Once a performance hard constraint gets added (currently none), or multi-user/multi-server demand becomes real.
