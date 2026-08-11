Status: v0 alignment snapshot | 2026-08-11 | settled

# ADR-0005: Public scope — fully public, single monorepo

## Context

The license (Apache-2.0) was already settled, but public scope (everything vs. core-only vs. private) was still undecided. This is a hard-to-reverse choice about whether internal-context scrubbing discipline applies to every commit and doc from this point on — the cost of walking back commit history after going public is high.

## Options considered and trade-offs

- **Core public only, the rest private**: sufficient for the "make the core engine reusable by other projects" goal (P0) on its own, but if mcp/cc stay private, nobody else can use docket as a whole — it doesn't spread.
- **Private for now, decide on going public after validation**: comfortable to work in without any public discipline at first, but converting to public later means cleaning up the entire commit history.
- **Fully public, single monorepo (adopted)**: all four layers developed in one public repo. No plan to split layers into separate repos.

## Decision

Settle on fully public, single monorepo.

## Consequences

**Gained**: other people can pick up all of docket (core+mcp+cc+console) right away. Avoids the history-cleanup cost of a later transition to public, entirely, from the start.

**Given up**: internal context that would otherwise get jotted down casually during dogfooding (machine names, concrete descriptions of real failure cases) can't go into commit messages, code comments, or the README — discovery context, consuming-project names, and paths are recorded only through separate internal channels (issue drafts, handoff messages).

## Re-open trigger

None — going public is a hard-to-reverse decision, so rather than revisiting the decision itself, we refine the operating discipline going forward on the assumption that the repo stays public.
