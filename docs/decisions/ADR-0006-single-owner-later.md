Status: v0 alignment snapshot | 2026-08-11 | tentative — re-open when: the auth approach is settled

# ADR-0006: Multi-user is Later, single-owner is the current premise

## Context

[principles.md](../principles.md)'s non-goals state "not a multi-user collaboration tool — the primary target is one person owning multiple machines." It was unclear whether this **permanently and structurally excludes** multi-user, or means **not now, but later**.

## Options considered and trade-offs

- **Permanent exclusion**: lock auth, permissions, and multi-tenancy entirely outside this project's scope. Even if team-scale demand shows up, it structurally can't be accepted.
- **Later (adopted)**: optimize for single-owner-multiple-machines for now, but since auth/tokens are already an open item that needs deciding, this doesn't structurally block multi-user either.

## Decision

Settle on Later. Don't add an "owner" concept to the core domain model (worker/topic/item) right now.

## Consequences

**Gained**: doesn't over-simplify auth design today with "it's just me, so who cares" — there's a good chance the core model won't need to be redesigned even if multi-user becomes necessary later.

**Given up**: closes off one simplification available at the auth/trust-boundary decision ahead of time — the option "single-user only, so no auth needed" is off the table.

## Re-open trigger

Re-open this ADR once the auth approach and token policy is settled.
