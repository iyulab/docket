Status: v0 alignment snapshot | 2026-08-11 | settled

# ADR-0001: Adopt the work-queue/kanban model

## Context

We need to settle how coordination is modeled among the many Claude Code sessions running across multiple machines at once. The metaphor for coordination (email / messenger / work queue) is a hard-to-reverse choice that determines an item's lifecycle, its failure modes, and its completion semantics all at once.

## Options considered and trade-offs

- **Email model**: the recipient (address) has to be resolved at send time. But the real request is "fix this problem," not "send it to a specific session on a specific machine." Bounce-back, out-of-office, and mailbox-lifecycle problems all come along with it.
- **Messenger model**: its premise is presence. Sessions die and come back constantly, so coordination evaporates every time presence drops. There's no completion semantics — threads are left dangling with no answer.
- **Work queue / kanban**: pull is the answer to "we don't know who will do it." An item survives even with no one around. The board is a direct representation of "whose court the ball is in." WIP limits naturally overlap with safeguards.

## Decision

Adopt the work-queue/kanban model. Items are created in front of a topic, survive without an owner, and get picked up by a worker via pull (`claim`).

## Consequences

**Gained**: requests can be addressed by subject instead of address. Coordination doesn't evaporate when a session dies. Completion semantics are explicit via the state machine.

**Given up**: real-time-ness ([principles.md](../principles.md) — explicitly given up as an axis it's fine to lose on). Conversational interaction with an immediate ack on send isn't possible in this model (that's split out as `question`, see [glossary.md](../glossary.md)).

## Re-open trigger

Re-open if assumption A-1 ("async coordination is good enough," [goals.md](../goals.md)) gets disproven during M2 (proof of existence) — i.e., if items frequently arrive already moot due to delay.
