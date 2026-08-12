# AGENTS.md

This document is the set of rules future sessions (agents) follow while working on docket. It has one purpose: **prevent arbitrarily settling a design question that hasn't been through an ADR.**

## Non-negotiable principle

**P-1. The core doesn't know its consumers.** No concept other than worker, topic, item, claim, body, stream, budget (see the [glossary.md](docs/glossary.md) mapping table for session, repo, ticket, hook, token budget, etc.) may appear in `docket-core`. If found in a PR/commit, reject it in review.

## Other principles (not non-negotiable, but flag violations)

- **P-2. Files are not the source of truth.** `docket-cc`'s file representation is only a projection of the core DB.
- **P-3. Not an orchestrator, pull only.** Don't add code that lets the center auto-distribute work to workers. A human's manual assignment (via the console) is the exception.

Full rationale: [principles.md](docs/principles.md).

## Non-goals (structurally out of scope)

- File-sync service
- Orchestrator (automatic distribution)
- Real-time chat
- Multi-user collaboration (Later — [ADR-0006](docs/decisions/ADR-0006-single-owner-later.md), don't move this up on your own)

## Current quality level

**Target: L1** — the typical-case scenarios ([vision.md](docs/vision.md) S1~S6) work end to end, even if only run manually. Claim exclusivity is treated as already covered by L0/L1, though — don't half-implement concurrent-claim handling just because "keep it simple" is the priority.

## Handling undecided design questions

Only Type-1 (hard-to-reverse) decisions get an ADR ([decisions/](docs/decisions/)) up front; most implementation details are deliberately left open until implementation forces a choice. That's not an oversight — don't treat "not in an ADR" as "undecided, so I get to pick." If implementation reaches a point where one of these needs deciding:

1. First confirm it actually needs deciding now (is there really no other way around it).
2. Once decided, document it — if it's Type-1-level, create a new `docs/decisions/ADR-NNNN-*.md`; otherwise leave the rationale at the call site (commit message or a short code comment).
3. Don't decide arbitrarily and move on without documenting it — the next session will run into the same question again.

## Public-repo discipline

This repo is fully public ([ADR-0005](docs/decisions/ADR-0005-public-scope.md)). Don't leave absolute paths, personal names, machine names, internal ticket IDs, or narration of discovery context in commit messages, code comments, or docs.
