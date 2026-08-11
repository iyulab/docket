Status: v0 alignment snapshot | 2026-08-12 | settled

# ADR-0008: File representation location — outside the repo

## Context

`docket-cc` (layer 3) projects each item onto a local file so a Claude Code session can read/write it without going through the HTTP API directly ([architecture.md](../architecture.md)). Where that projection physically lives is a hard-to-reverse choice: once files exist at a location and a session's workflow depends on finding them there, moving the root means migrating every existing projection and re-pointing whatever reads it.

## Options considered and trade-offs

- **Inside each repo** (e.g. a `.docket/` directory at the repo root): visible right where the work is happening, but every repo needs its own `.gitignore` entry — a maintenance burden that scales with the number of repos docket manages, and one missed entry away from a projection landing in git history. A dangling or deleted repo takes its projection with it, and there is no single place to look across every topic a worker owns.
- **Outside the repo, in a user data directory**, mirroring topic paths (e.g. `~/.docket/<topic>/...`): structurally cannot be committed or pushed by accident — no `.gitignore` discipline required per repo. Survives repo deletion/re-clone. One location holds every topic's projection regardless of how many repos are checked out locally, which matches the single-owner/multiple-machines target ([scope.md](../scope.md)). Trade-off: the projection isn't physically visible while working inside a given repo — a session (or a human) has to know the mapping to find it.

## Decision

**The file representation root lives outside the repo, in a user data directory**, with the physical layout mirroring the topic path (`<root>/<topic>/...`).

This directly reinforces P-2 ([principles.md](../principles.md) — files are not the source of truth, the core DB is): a location that cannot be accidentally version-controlled makes it structurally harder to mistake the projection for the source of truth.

## Consequences

**Gained**: no per-repo `.gitignore` maintenance burden, no risk of a projection leaking into a public repo's git history, projections outlive the repos they're about, and one root gives a single vantage point over every topic a worker owns.

**Given up**: the projection isn't visible next to the code by default — finding `<root>/<topic>/...` requires knowing the topic-to-path mapping, which becomes `docket-cc`'s job to make discoverable (surfacing the resolved path, or a lightweight lookup) rather than the filesystem doing it for free via proximity.

## Re-open trigger

If in practice sessions need the projection to be discoverable by mere proximity (browsing the repo directly, no separate lookup step) and `docket-cc` can't make the out-of-repo path discoverable enough to compensate, or if a per-repo layout turns out to be required for reasons not yet foreseen.
