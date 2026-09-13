Status: v0 alignment snapshot | 2026-08-23 | settled

# ADR-0017: `force-approve` — a fourth admin close for out-of-band completion

## Context

A real deployment reported a live case: an assignee worker completed real work end-to-end
(triage, implementation, deployment) but never called `claim_item`/`submit_item` — it narrated
every step through `add_comment` instead. The item stayed `state=open`, `assignee=null`. When the
requester tried `approve_item` after confirming the work, the call was rejected with `conflict:
cannot approve: item is open` — `approve_item` requires `resolved`
([ADR-0003](ADR-0003-item-state-schema.md)), which this item never reached.

The three existing admin closes ([ADR-0012](ADR-0012-item-reject-reopen-transitions.md)) already
give an admin a state-unrestricted way to end an item early — but all three are negative
dispositions (`invalid`/`duplicate`/`wontfix`, [ADR-0015](ADR-0015-merge-duplicate-of-reference.md)'s
"every other admin close names a reason a reader can act on directly"). None of them lets an admin
say "this was actually done, just not through the formal `claim`/`submit` path" — the one case this
issue reports is exactly that gap. `docs/goals.md` already anticipates a `force-close`-shaped
concept counting as "workflow failed" intervention; a positive-completion sibling was simply never
built.

This is squarely inside `docket`'s own state-machine responsibility, not a consumer-domain concept:
any third-party worker that narrates completion purely through comments — forgetting
`claim`/`submit` — leaves its requester in the same dead end (`라이브러리 한계 = 개선 기회`).

## Options considered and trade-offs

**Whether to relax an existing primitive or add a new one:**

- **Reject — let `submit_item`/`approve_item` be called without a matching `claim`**: breaks the
  invariant `submit` checks (`assignee` must match the caller) and `approve`'s `resolved`-only gate
  — both exist to keep the two-party `claim → submit → approve` handshake honest. Weakening them to
  unblock this one case reopens the door to any worker skipping `claim` routinely, not just this one
  late-recovery scenario.
- **Reject — let the requester call `claim_item`/`submit_item` on the assignee's behalf**: the
  issue reporter already ruled this out correctly — it impersonates another party's `worker_id`,
  corrupting the very attribution `claim`/`submit` exist to record. Not implemented, not proposed
  here either.
- **Accept — a fourth state-unrestricted admin close, `force-approve`, `resolution = done`**
  (adopted): same shape as `remove`/`merge`/`force-close` — an admin/human value judgment about an
  item's disposition, not a worker action, so it doesn't touch the worker-facing
  `claim`/`submit`/`approve` handshake at all. `Resolution::Done` already exists (used by
  `approve_item`); no new enum value.

**MCP exposure:**

- **Reject — expose as a `docket-mcp` tool**: would let any worker approve its own (or another
  item's) completion without ever reaching `resolved` — precisely the self-approval shortcut the
  existing `claim → submit → approve` split exists to prevent, and precisely the workaround the
  issue itself flagged as unacceptable in the "claim/submit on the assignee's behalf" option above.
- **Accept — HTTP/console-only, like `remove`/`merge`/`force-close`** (adopted): matches
  [architecture.md](../architecture.md)'s MCP-exposure rule exactly — "an admin/human value
  judgment about an item's disposition" stays HTTP-only. A human operating the console, not an agent
  acting on its own judgment, is the one who confirms out-of-band completion.

**Distinguishing normal completion from an override, given both write `resolution = done`:**

- **Reject — a separate `resolution` value (e.g. `done-forced`)**: grows the enum for a distinction
  `resolution` was never meant to carry — *why*/*how* an item closed already lives in the lifecycle
  comment each admin close writes (`insert_lifecycle_comment(..., "force-close", ...)` etc.), not in
  `resolution` itself.
- **Accept — same `resolution = done`, distinguished by the lifecycle comment's op name
  (`"force-approve"` vs `"approved"`)** (adopted): consistent with how every other admin close
  already records its own identity, and keeps `resolution`'s four-value enum exactly as
  `docs/architecture.md` documents it today. A reader (or `goals.md`'s completion-rate metric) that
  needs to tell a confirmed completion from an admin override reads the op name off the lifecycle
  comment, not `resolution`.

## Decision

```
(any pre-closed state) --force-approve(author)-> closed(done)   # new, HTTP/console-only, no MCP tool

Store::force_approve_item(id, author) = close_with_resolution(id, Resolution::Done, "force-approve", author)
  # identical shape to remove_item/force_close_item — same any-pre-closed-state,
  # assignee-agnostic rules, same shared helper, no new Resolution variant.

POST /items/{id}/force-approve {"author"?}   # bodiless-optional, same as remove/force-close
```

`docket-console`'s item detail view gains a fourth button in the existing admin-actions row (next
to Remove/Force-close). No `docket-mcp` tool. `docs/usage.md`/`docs/architecture.md`/`README.md`
document it alongside the other three admin closes.

## Consequences

**Gained**: a requester is no longer permanently stuck when an assignee completes real work but
never calls `claim_item`/`submit_item` — the exact failure Issue #29 reported. The fix reuses an
already-proven pattern (three siblings, one shared helper) rather than inventing new state-machine
surface, and stays out of `docket-mcp` entirely, so no worker gains a new way to self-approve.

**Given up**: `resolution = done` alone no longer implies "the requester approved through the
normal `claim → submit → approve` path" — a caller that needs to distinguish normal completion from
an admin override must read the lifecycle comment's op name, not just `resolution`. `docs/goals.md`'s
completion-rate metric (`resolution=done` counted as normal, `force-close` counted as intervention)
must do the same for `force-approve` — treat it as an intervention signal like `force-close`, not as
evidence of an unaided completion.

## Re-open trigger

If `force-approve` ends up used routinely rather than as a rare recovery path, that is itself a
signal that workers are skipping `claim`/`submit` often enough to need a first-class fix on the
worker side (Issue #29's option 1: a heuristic warning, or option 3: stronger `docs/usage.md`
guidance) — revisit then, gated on the same real-usage evidence [B-04] already gates
`claimed`-substate work with.
