Status: v0 alignment snapshot | 2026-08-11 | settled

# ADR-0002: Four-layer separation and the core's consumer-agnostic principle

## Context

docket's real-world motivation is coordinating multiple Claude Code sessions, but its design goal, from the start, was to make "the core engine reusable by other projects too." For these two goals not to conflict, AI-specific concepts (sessions, hooks, token budgets) must never mix with general-purpose work-queue concepts (workers, topics, items, claims).

## Options considered and trade-offs

- **Single layer (monolithic)**: Claude Code knowledge goes straight into the core. Fast to implement, but if another runtime (like `aims`) wants to reuse the core, that knowledge has to be stripped back out — giving up the exact goal stated in the motivation above.
- **Two layers (core + Claude Code adapter)**: MCP (pull) and hooks (push) are mechanically different, but if they're mixed into one adapter, "the core doesn't know its consumers" ends up blurred again inside that adapter.
- **Four layers (core/mcp/cc/console), split first by human vs. AI consumer, then by pull vs. push**: the layer boundary lands exactly on the mechanism boundary (pull vs. push) — a signal that the split isn't arbitrary.

## Decision

Split into four layers: `docket-core` (agnostic) → `docket-mcp` (pull, AI in general) / `docket-cc` (push, Claude Code) → `docket-console` (human). Keep it as a single repo, but force leak prevention with three mechanisms: dependency-direction checking, no Claude Code in core tests, and locked vocabulary ([glossary.md](../glossary.md)). Treat `P-1` (core doesn't know its consumers) as non-negotiable.

## Consequences

**Gained**: other agent runtimes, human workers, other topic conventions, and `aims` extension all become possible with zero core changes ([architecture.md](../architecture.md) extension points).

**Given up**: the shortcut of dropping a Claude-Code-specific feature straight into the core. We keep paying the cost of translating it up to layer 3 every time instead ([principles.md](../principles.md), P-1's "when it gets expensive").

## Re-open trigger

Re-open if any one of the three enforcement mechanisms turns out to be mechanically impossible to implement within a single repo (e.g. no dependency-direction-checking tool can be found).
