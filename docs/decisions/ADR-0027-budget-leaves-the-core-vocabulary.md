Status: v0 implementation | 2026-10-29 | implemented

# ADR-0027: `budget` leaves the core vocabulary

## Context

[principles.md](../principles.md) P-1 closes the core's vocabulary to a short list of nouns, and
`budget` has been on it since the first alignment snapshot, glossed as "token budget"
([glossary.md](../glossary.md)). [architecture.md](../architecture.md) lists it among the core's
concerns and the M4 roadmap entry names "the full budget mechanism" as work to do.

Nothing implements it. The word appears in no Rust or TypeScript source; the core has no counter, no
limit and no field it would apply to. It was written down for a mechanism that was designed early and
never built: a session extension that would keep a Claude Code session working through item after item
until a token allowance ran out, with A-3 in [goals.md](../goals.md) asking whether that cap would stop a
runaway loop.

Two things have since settled that make the word wrong rather than merely unused:

- **The product is pull-only** ([P-3](../principles.md)). `docket-cc` syncs a projection when a session
  starts and `docket-mcp` answers calls; neither keeps a session going. There is no loop in docket to
  run away, so there is nothing for a cap to guard.
- **A token is a model's concept.** P-1's reason for existing is that the core would exist
  unchanged without AI: a queue and a board for headless workers. A "token budget" in the core's
  vocabulary is the kind of consumer concept the vocabulary list is there to keep out.

## Decision

- Remove `budget` from P-1's vocabulary list, from the glossary's translation table, and from the
  architecture table. This reverses nothing that exists; it removes a word that never had an
  implementation.
- Withdraw A-3. Its question has no subject while nothing extends a session.
- Drop "the full budget mechanism" from M4.

## What this does not decide

The "max depth", "max comments per item", "max creation rate" and "max open items per topic"
questions that sit under M4 are unrelated to tokens: they would be limits the *core* enforces on the
queue itself. They stay unbuilt and undecided, and each is taken on evidence of an actual flood, not
because M4 once listed safeguards.

## Re-open when

A limit that only the core can enforce becomes necessary — say, a rate limit on item creation. It
should be named for what it limits, not revived as `budget`: the word carried a meaning (tokens) that
does not belong in the core. A worker loop that limits its own repetition is the loop owner's concern,
in whatever adapter builds one.
