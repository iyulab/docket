Status: v0 implementation | 2026-09-22 | implemented

# ADR-0025: Enumerating the identity class, and detecting a drifted spelling

## Context

[ADR-0021](ADR-0021-case-insensitive-identity.md) established `worker id`, `requester`, `assignee`
and `topic` as one identity class and folded case across it;
[ADR-0022](ADR-0022-identity-alias.md) generalized that fold to declared aliases, so two spellings
a human says are one identity become one identity everywhere, retroactively. Between them, and
with `set_item_requester`/`set_item_assignee`/`set_item_topic` for a single row, the means to
resolve a spelling drift are complete.

**What neither provides is a way to find out that one happened.** Both ADRs were written from
drift discovered by accident — a consumer noticing its own items missing, and a second case found
while measuring the first. `put_alias` and `set_item_requester` both run only *after* somebody has
already noticed; nothing reports that two spellings of one party exist.

The `topic` arm of the class does not have this problem, because three separate devices were added
to it for reasons of their own: `list_topics` enumerates every topic with its declared aliases,
`list_topics.owned_by` marks a topic no registered worker covers, and
`GET /topics/candidates` reports a topic sharing another's last segment
([ADR-0022](ADR-0022-identity-alias.md), "The mistargeting detector's segment-equality rule"). The
party arms — `requester`, `assignee`, `worker id` — have no counterpart to any of the three. A
`requester` that drifted is reachable by no query at all: `list_items(requester=…)` under either
spelling returns that spelling's rows and says nothing about the others.

**Measured, not anticipated.** A survey of a live deployment (350 non-archived items, 62 topics,
35 registered workers, zero declared aliases) found **11 groups of distinct identities sharing a
last segment**, spread across every field of the class:

| Shape of the pair | Groups | Example |
|---|---|---|
| bare short form vs scoped form | 8 | `widget-works` and `acme/widget-works`, 74 items vs 2 |
| two different scopes, same leaf | 3 | `acme/booster` and `other-org/booster` |

Only **5** of the 11 have both spellings appearing as a `requester`; the other 6 have one spelling
in one field and the other in a different one — most commonly a party that files as
`acme/board-umbrella` and claims as `board-umbrella`. In **5** groups neither spelling is a
registered worker at all. Both numbers decide options below.

## Options considered and trade-offs

- **Per-field enumeration (`list_requesters`, the shape originally requested)** — rejected on the
  measurement. It re-splits, at the read surface, the namespace ADR-0022 deliberately unified, and
  the split is not cosmetic: a per-field listing surfaces 5 of the 11 observed groups and omits the
  other 6 entirely, because for those the second spelling is never a `requester`. The very drift
  that is hardest to notice — a party whose two spellings sit in two different fields — is the one
  this shape cannot see.
- **A standing "all collisions" report, or a scheduled sweep** — rejected. A report that lists
  every collision on the server has to be *right* about which pairs are real, because it is shown
  without being asked for; and it cannot be, since two parties legitimately sharing a last segment
  is normal. It would nag, and the response to nagging is to stop reading it.
- **A "declared distinct" table, so a legitimate pair can be silenced** — rejected, and it is the
  natural next thought after the previous option, which is why it is recorded here. It would split
  the single human declaration ADR-0022 settled on (`put_alias`: *these are one identity*) into
  two mirror-image declarations that then have to be kept consistent, and it only earns its
  complexity if something nags. Nothing does — see the decision below.
- **Porting `topic_candidates`' unserved gate to the party arms** — rejected; there is nothing to
  port it to. That gate means "no registered worker covers this topic", which is evidence a topic
  was mistargeted. A `requester` is not expected to be a registered worker, so the same test says
  nothing about a party identity — and in 5 of the 11 measured groups neither spelling is
  registered, so a ported gate would report both halves of every one of those pairs as suspicious,
  or, applied the other way, suppress them all. Precision has to come from somewhere else.
- **Similarity scoring (edit distance) over identities** — rejected for the reason ADR-0022
  already gave for topics: it is not a finer-grained version of segment equality, it is a
  different and unprincipled rule, comparing identifiers as arbitrary strings with no regard for
  where their structure is.
- **Enumeration over the whole class, plus a request-scoped detector (adopted)** — below.

## Decision

**The identity class gets the same two discovery surfaces `topic` has, defined over the whole
class rather than one field of it, and precision comes from the question being scoped to one
identity rather than from a gate.**

### `GET /identities` — the counterpart of `GET /topics`

One row per identity, carrying its declared aliases and how many non-archived items use it as
`requester`, as `assignee` and as `topic`, plus whether a worker is registered under it. Folding is
`list_topics`' exactly: declared aliases fold into their canonical (ADR-0022), case variants fold
together (ADR-0021), and the surviving spelling is the lexicographically-first one — decided by
sorting before the fold rather than left to SQL row order.

**Its population is `items`' three identity columns *union* `workers.id`**, and this is the one
place it deliberately diverges from `list_topics`, which is a `GROUP BY` over items alone. A worker
whose registered spelling drifted receives no items *by definition* — the drift is exactly what
stops items reaching it — so an items-only enumeration would omit the most severe case from the
surface built to find it. Zero items is the symptom, not a reason to drop the row.

### `GET /identities/candidates?identity=` — the counterpart of `GET /topics/candidates`

Identities whose last `/` segment equals the queried one's, excluding the queried identity's own
alias group. Same exact, unscored rule; **no unserved gate**, per the option above.

There is no `unserved` field in the response either. `/topics/candidates` carries one because
served-ness is real evidence there; here there is no truthful value to put in it, and a field that
means something different on a sibling endpoint is worse than an absent one.

### Precision comes from request scope, not from a gate

Both surfaces answer a question the caller asked: the enumeration is a listing a human reads
deliberately, and the detector is *about one identity the caller named*. Neither arrives unbidden,
so neither has a false-positive budget to defend — which is what makes the rejected "declared
distinct" table unnecessary rather than merely unbuilt. A pair that is genuinely two parties stays
reported, forever, and costs nothing, because nobody is shown it who did not ask.

The one delivery surface that *is* unbidden — the `report_drift` hint the MCP layer adds to
`list_items`/`search_items` — stays inside the same rule: it is opt-in, and it requires the query
to already name an identity (`mine`, `requester` or `assignee`), so what it reports is bounded by
what the caller asked about. With no identity in the query it is a no-op, not a dump of every
collision on the server.

### Still advisory, still nothing automatic

Nothing here rejects, rewrites, folds or auto-corrects. The repair paths are unchanged and both
remain human-initiated: `set_item_requester`/`set_item_assignee`/`set_item_topic` for one item,
`put_alias` for a recurring spelling. `put_alias` **stays off MCP** for the reason ADR-0022 gave —
it grants standing approval authority across a whole class of items. `list_identities` *is* exposed
on MCP, because reading a list grants nothing; the exposure rule is about blast radius, not about
which concept a call touches.

### `list_topics` and `/topics/candidates` are unchanged

This ADR adds surfaces; it does not alter the topic ones. They answer a different question —
*is this topic outside everyone's jurisdiction* — which is about coverage, not spelling. The two
share one primitive (last-segment equality over alias-folded identities) and nothing else.

## Consequences

**Gained**: a spelling drift is now findable without already suspecting it. The enumeration makes
the whole class readable in one call, in the form where two spellings of one party sit next to
each other; and a query that came up short can be asked, in place, whether the missing rows are
filed under another spelling.

**Given up — nothing is asserted to be the same.** These surfaces report a *coincidence of
structure*, never an identity. Acting on one is still a human judgment, and deliberately so: the
3 measured cross-scope groups could each be a genuine organization move or a genuine drift, and no
rule available to the core can tell which.

**Given up — a legitimate pair is reported indefinitely.** There is no way to mark two identities
permanently distinct. This is the cost of not building the "declared distinct" table, and it is
cheap only because nothing nags; if a standing report is ever added, this trade has to be reopened
with it.

**Not changed**: tags (untouched by every identity rule, ADR-0021/0022), item ids and `seq`
aliases (a different mechanism, [ADR-0016](ADR-0016-item-seq-alias.md)), and the resolution
semantics of the alias table itself.

## Re-open trigger

A standing or scheduled collision report being genuinely wanted — that is the change that would
make the unbuilt suppression mechanism necessary, and it should be decided together with it, not
after. Separately: an identity whose drift is *not* a last-segment match (a rename that changes
the leaf, which segment equality cannot see by construction) appearing in practice, since that is
the case this rule is known not to cover.

## Related

[ADR-0021](ADR-0021-case-insensitive-identity.md) and
[ADR-0022](ADR-0022-identity-alias.md) define the identity class and the two folding rules this
enumerates over; ADR-0022 also holds the reasoning for last-segment equality, which this extends
from `topic` to the whole class.
