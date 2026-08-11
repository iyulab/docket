Status: v0 alignment snapshot | 2026-08-11 | settled

# ADR-0003: Item state / resolution schema

## Context

The initial design used an `open → claimed → review → done → dropped` state machine. External review flagged two problems: (1) `review` risks being misread as code review in a development context, and (2) `dropped` crams a separate axis — "why it closed" — into a single state.

## Options considered and trade-offs

- **Keep the original (`review`/`done`/`dropped`)**: no cost to change, but `review`'s ambiguity is likely to keep causing confusion in real use.
- **Rename only (`review→resolved`, `done→closed`), keep `dropped`**: less ambiguous, but "why did it close" (normal completion/duplicate/declined/invalid) still stays mixed into the state value, so the state count keeps growing over time.
- **Split `state`/`resolution` + rename (adopted)**: Bugzilla/Jira convention. State expresses only the workflow stage; the reason it closed is pulled out into a separate field. Each admin operation (remove/merge/force-close/approve) gets exactly one clear `resolution` value.

The rename itself is adopted, but the external review's proposed `resolution` mapping (remove→`wontfix`, force-close→`invalid`) didn't match §11.4's definitions of those operations, so it was corrected — "remove" is a request that was invalid from the start (`invalid`), while "force-close" was once valid but is no longer relevant (`wontfix`).

`expired` isn't included in this schema — automatic claim expiry and automatic stall-closing policy are both still undecided, and adding the field now would let the schema implicitly settle a decision that hasn't actually been made.

## Decision

```
state: open | claimed | resolved | closed
resolution: null | done | duplicate | wontfix | invalid   # only when closed
```
Mapping: remove→`invalid`, merge→`duplicate`, force-close→`wontfix`, requester approval→`done`.

## Consequences

**Gained**: the state count drops from 5 to 4, and all four admin operations get a clear meaning. The state names are effectively identical to Bugzilla's lifecycle, so even a first-time reader understands them without explanation.

**Given up**: closure due to automatic expiry can't currently be expressed — a stalled item still has to be force-closed (`wontfix`) manually by a human.

## Re-open trigger

Add `expired` to `resolution` once [open-questions.md](../open-questions.md) #14/#16/#19 (claim expiry, automatic stall-closing) are decided.
