Status: v0 alignment snapshot | 2026-08-24 | implemented

# ADR-0019: `approve`/`reject` require caller identity to match `requester`

## Context

[ADR-0010](ADR-0010-item-from-to-turn.md) and [ADR-0011](ADR-0011-requester-assignee-naming.md)
gave items a two-party handshake — `requester`/`assignee`/`turn` — and [ADR-0012](ADR-0012-item-reject-reopen-transitions.md)
gave every closing operation an `author` field so a closure carries a record of who performed it.
`author`, at the time, was purely a traceability annotation: it is written into a lifecycle
comment and never read back by any `WHERE` clause.

`submit_item` does not share that property. `claimed -> resolved` requires the caller's
`worker_id` to equal the item's `assignee`:

```rust
// crates/docket-core/src/storage.rs:423-426
"UPDATE items SET state = 'resolved', updated_at = ?1
 WHERE id = ?2 AND state = 'claimed' AND assignee = ?3"
```

`approve_item`/`reject_item` — the mirror-image requester-side transitions — check only
`state = 'resolved'`; `author` is written to the comment but never compared to `requester`:

```rust
// crates/docket-core/src/storage.rs:509-513
"UPDATE items SET state = 'closed', resolution = 'done', updated_at = ?1
 WHERE id = ?2 AND state = 'resolved'"
```

The practical consequence: any caller holding an MCP session (i.e. any registered worker) can
chain `claim_item -> submit_item -> approve_item` under its own `DOCKET_WORKER_ID` and close an
item nobody but the assignee itself ever looked at as "requester-approved." Nothing in `docket-core`
distinguishes that from a real approval. This was found by asking a broader question first —
"if an assignee closes an item, how would the requester ever miss it?" — and narrowing it down to
this specific gap after finding that the four admin closes (`remove`/`merge`/`force-close`/
`force-approve`) are console/HTTP-only with no MCP tool (`docs/usage.md`'s "Four more HTTP-only
operations" note), so they were never the actual MCP-reachable exposure.

This is an asymmetry, not a deliberate design choice: [principles.md](../principles.md)'s
quality-attribute priority section calls out that "claim exclusivity is part of the domain
model's own definition ... so it's never sacrificed in the name of 'keep it simple'." Approve
exclusivity is the same shape of invariant on the other party of the same handshake, and
`principles.md`'s P-1 ("core doesn't know its consumers") argues the invariant belongs in
`docket-core` itself — enforcing it only in `docket-mcp` would leave `docket-console`, any raw
HTTP caller, and any future client to each either reimplement the same check or silently reopen
the gap, making a core domain rule into something every consumer has to separately remember.

The one caller not designed for this: `docket-console` sends a hardcoded literal for every
closing write, not a real per-actor identity —

```ts
// console/src/api.ts:147-150
// The four closing operations record who closed the item (ADR-0012). The
// console has no per-user identity — every write from here is a button click
// in the single-owner admin UI — so it attributes them to a fixed `console`
// author rather than leaving the server's "unknown" fallback to stand in.
const CONSOLE_AUTHOR = 'console'
```

— which was a reasonable shortcut while `author` was comment-only, but would make every console
approve/reject fail once `author` becomes load-bearing. This is not [ADR-0006](ADR-0006-single-owner-later.md)'s
"no owner concept in core" boundary reopening: `requester`/`assignee` already exist in the core
domain model (and `assignee` is itself a rename of an earlier `owner` field per ADR-0011) — this
ADR asks the console to stop hardcoding a placeholder into an already-free-text field, not to gain
login, sessions, or roles.

## Options considered and trade-offs

**Where the check lives:**

- **`docket-mcp` only**: matches the shape of the existing HD-16/HD-17 tightening (MCP requires
  identity to be *resolvable* where `docket-core` leaves `author` optional). Rejected: that
  precedent is about identity *presence*, not an authorization invariant, and P-1 argues a real
  domain rule belongs in the one place that owns the domain model. `docket-console` and any other
  HTTP caller would stay exposed, and the rule would live as MCP-client convention rather than a
  property of the item itself.
- **`docket-core`, mirroring `submit_item`'s existing pattern** (adopted): one enforcement point,
  the same shape as the invariant `principles.md` already says is never sacrificed. Every current
  and future caller — MCP, console, raw HTTP — gets the same guarantee for free.

**What happens when `requester` is `null`:**

- **Reject approve/reject until `requester` is set**: closes the loop tighter, but breaks routine
  use — many items are filed without a requester and there is no requirement to backfill one
  before work starts.
- **Pass through unchanged when `requester` is `null`** (adopted): there is nothing to violate —
  no party was ever named to hold this turn. `set_item_requester` (ADR-0011) remains the way to
  attach one after the fact if tighter enforcement is wanted for a specific item.

**Whether the four admin closes (`remove`/`merge`/`force-close`/`force-approve`) get the same
check:**

- **Yes, symmetric treatment**: rejected — these exist specifically as an override valve
  ([ADR-0012](ADR-0012-item-reject-reopen-transitions.md), [ADR-0017](ADR-0017-item-force-approve.md))
  for exactly the cases where the normal handshake didn't happen or shouldn't be waited on
  (mistaken filing, duplicate, no longer relevant, narrated via comments only). Requiring
  `requester` match here would defeat the reason they exist.
- **No, leave assignee/requester-agnostic** (adopted): unchanged from today. `docket-console`
  gaining a real `author` still benefits these four ops' traceability (a meaningful actor string
  instead of the fixed `"console"`), without changing who may call them.

**Whether `reopen_item` gets the same check:**

- **Rejected**: [ADR-0012](ADR-0012-item-reject-reopen-transitions.md) already decided `reopen` is
  callable by any worker on purpose, and an unwanted reopen is visible noise (an item unexpectedly
  back in `claimed`), not a silent, undetectable loss the way a bad `approve` is. Out of scope
  here.

**How `docket-console` supplies a real `author`:**

- **Add a login/session/role system**: rejected outright — reopens [ADR-0006](ADR-0006-single-owner-later.md)
  for no benefit this ADR needs.
- **A user-set "acting as" value, stored client-side (e.g. `localStorage`), sent as `author` on
  every write, replacing the hardcoded `CONSOLE_AUTHOR`** (adopted): no verification, no session,
  no schema change — the same "opaque, caller-defined string" treatment `requester`/`assignee`
  already get everywhere else. Scoped to `docket-console`; out of scope for this ADR to design in
  full (tracked as a follow-up implementation task, not a design question).

## Decision

`approve_item`/`reject_item` add a `requester` match to their existing `WHERE` clause, matching
`submit_item`'s `assignee` check:

```
approve_item: WHERE id = ? AND state = 'resolved' AND (requester IS NULL OR requester = ?)
reject_item:  WHERE id = ? AND state = 'resolved' AND (requester IS NULL OR requester = ?)
```

A mismatch returns the same `existing_state_conflict`-shaped error path `claim_item`/`submit_item`
already use for a failed `WHERE`, with a message distinguishing "wrong state" from "wrong
requester" so a caller can tell an invariant violation from a stale-state retry, and pointing at
`set_item_requester` when the mismatch looks like a drifted identity rather than a genuine
wrong-party call.

`remove`/`merge`/`force-close`/`force-approve`/`reopen` are unchanged — still assignee- and
requester-agnostic, per the admin-override rationale above.

`docket-console` must stop sending the hardcoded `CONSOLE_AUTHOR` literal once this lands, or
every console approve/reject on a `requester`-bearing item will fail. The console-side identity
mechanism is implementation detail for the follow-up work, not re-litigated here.

## Consequences

**Gained**: "only the requester may approve/reject" becomes a real, single, core-enforced
invariant — the same class of guarantee `principles.md` already grants claim exclusivity — instead
of a documented convention that only a well-behaved MCP client happens to follow. Closes the
MCP-reachable path where an assignee's own session could self-approve its own submitted work.
Every current and future `docket-core` caller inherits the guarantee without reimplementing it.

**Given up**: a `requester`/worker-id string mismatch (typo, a repo renamed without updating the
item, inconsistent naming convention between two consumers) now hard-fails a legitimate approve —
mitigated by `set_item_requester` and a clear error message, not eliminated.

**Implemented** (2026-08-24, same day as this decision): `storage.rs`'s `approve_item`/
`reject_item` add the `requester` match with a dedicated `approve_reject_conflict` helper that
distinguishes wrong-state from wrong-requester in the error message; six new `docket-core` tests
cover match/mismatch/null-passthrough for both ops (all pre-existing approve/reject tests used
`requester = null` fixtures and pass unchanged). `docket-console` replaced its hardcoded
`CONSOLE_AUTHOR` literal with a per-browser, user-settable "acting as" identity
(`identity.ts`, `localStorage`-backed, default `console`) wired into every closing op via
`api.ts`, with an input in the app header — landed in the same batch, not independently.
`docket-mcp`'s `approve_item`/`reject_item` tool descriptions were updated to mention the
requester-match requirement (the HTTP forwarding itself needed no change — `author` already passed
straight through). Verified via new unit tests, a live MCP stdio handshake (match/mismatch/
null-passthrough for both `approve`/`reject`), and a real-browser console walkthrough (mismatched
"acting as" shows the conflict banner inline; matching it approves successfully).

**Explicitly not addressed by this ADR**: visibility. Even with this enforced, a requester who
never queries `list_items(requester=me, state=closed)` can still miss a legitimate admin
override (`force-close`, `remove`, `merge`) closing an item they cared about — those remain valid,
agnostic, human-console actions by design. That gap is a query-convention concern, not a
`docket-core` invariant, and is out of scope here.

**2026-08-24 update — the query-convention gap above is documented, not left implicit**
(`docket-works#32`): no new primitive was needed — `list_items(requester=<id>, state="closed")`
already answers "what closed while I wasn't looking" with the same tools this ADR's own text names.
What was actually missing was the connection between that existing call and `mine`'s deliberate
`closed` exclusion. `docs/usage.md` §5 (the worker loop) now states this explicitly next to `mine`,
so a reader lands on the answer instead of re-deriving it the way this ADR's own investigation had
to.

## Re-open trigger

If `requester`/worker-id string drift turns out to be common enough that legitimate approvals are
routinely blocked (not just theoretically possible), revisit whether the match should be
case-insensitive, fuzzy, or backed by a real registered-worker reference rather than a bare string
compare — that would be a schema-level change to how `requester` is stored, not just this
invariant's check.
