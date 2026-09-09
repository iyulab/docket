Status: v0 implementation | 2026-09-09 | implemented

# ADR-0022: Declared identity aliases fold at comparison time

## Context

[ADR-0021](ADR-0021-case-insensitive-identity.md) established that `worker id`, `requester`,
`assignee` and `topic` are one identity class — strings whose only job is to say whether two
references mean the same thing — and that two spellings differing only in case are the same
identity. Case is not the only way two spellings name the same thing. A repository gets renamed;
an org migrates; a caller has always used a short form (`widget`) where another uses the full,
scoped one (`acme/widget`). None of these are typos, and none are case drift — they are a
declaration only a person can make: *these two spellings are the same identity*.

Without a way to make that declaration, the party filing under the old or short spelling and the
party querying under the new or full one silently diverge, the same failure mode ADR-0021 fixed
for case — except case folding cannot help here, because neither spelling is wrong and there is no
mechanical rule (lowercase, trim, normalize) that derives one from the other. Only a human
decision does, and the mechanism needs a place to record that decision once it's made.

The identity class this generalizes to includes `topic`, so it also resolves a standing open
question: how a topic is migrated when the underlying repository is renamed. The answer this ADR
gives is that it isn't migrated at all — the old name is declared an alias of the new one, and
every existing item filed under the old name is reachable, and its jurisdiction owned, exactly as
if it had always been filed under the new one.

## Options considered and trade-offs

- **Normalize on write (rewrite the stored spelling to its canonical form)** — rejected. This
  contradicts the property [ADR-0021](ADR-0021-case-insensitive-identity.md) already committed to
  — storage keeps what was written — for the same reason ADR-0021 rejected it for case: an item
  shows the spelling its filer actually used, and rewriting it on the way in destroys that
  information rather than fixing a comparison. It also cannot apply retroactively: a rename
  declared today cannot reach back and rewrite items filed months ago without a migration that
  touches every affected row, and every row filed after the declaration but before whatever process
  performs the rewrite would still need it applied at read time anyway — the write-time fix ends up
  needing the read-time one behind it regardless.
- **Per-consumer read-side fan-in (each consumer queries every known spelling and unions the
  results itself)** — rejected. This leaves storage permanently fragmented — the two spellings are
  still two different values as far as any comparison not written by that consumer is concerned —
  and it pushes the union onto every caller: the console, `docket-cc`, any future adapter, each
  reimplementing the same "which spellings does this identity have" lookup, or worse, each getting
  it slightly wrong in a different way. [principles.md](../principles.md) P-1 puts one domain rule
  in one place for exactly this reason; an identity's alternate spellings are as much a domain fact
  as the identity itself, and a library that makes every consumer re-derive one of its own domain
  facts has pushed its own responsibility onto its callers.
- **Fold at comparison time, storage preserved (adopted)** — the same shape ADR-0021 already
  chose for case, generalized from a fixed folding rule (`eq_ignore_ascii_case`) to a caller-declared
  one (a lookup table). One comparison primitive resolves a spelling to its canonical form before
  every comparison; nothing already stored is touched, and the fold applies to every item filed
  under the old spelling before the declaration existed, not only ones filed afterward. Declaring an
  alias is therefore retroactive by construction, with no migration step and nothing to backfill.

## Decision

**A declared alias names an identity, not a spelling.** `identity_aliases` rows say "`alias` is
`canonical`, spoken differently"; comparison resolves both sides of every identity check through
this table before comparing, exactly as it already resolves case (ADR-0021); storage keeps
whichever spelling a caller actually wrote, on old rows and new ones alike.

### Invariants

- **Resolution is always one hop.** An alias may never itself be another row's canonical, and a
  canonical may never be another row's alias — chains are rejected at declaration time, not merely
  discouraged. This is what lets every resolution be a single table lookup with no loop or cycle
  guard anywhere a resolution happens; a chain would turn every one of those sites (there are more
  than a dozen) into a place that has to reason about how many hops to follow and what happens if
  one is missing.
- **One alias maps to exactly one canonical.** Declaring an alias that already points somewhere
  else is a conflict, not a silent repoint. An alias resolves to a fixed identity everywhere it's
  used — including inside the authorization checks below — so quietly moving what it means would
  retroactively move every item filed under it to a different party without anyone who filed or
  approved under the old meaning being told.
- **No self-alias.** An identity cannot be declared its own alias; this is rejected as malformed
  input, not a state conflict, since it asserts nothing the mechanism can act on.

### One namespace

`worker id`, `requester`, `assignee` and `topic` share one alias table and one resolution
function, the same single class ADR-0021 already established for case folding. A declared alias
between two org/repo-shaped strings folds all four uses of that string at once — there is no
separate "topic alias" versus "party alias" to keep in sync, because the class was never split to
begin with.

### Scope boundary: an alias is not shared authority

**An alias declares that two spellings name the same identity. It does not grant one identity
authority over a different one.** An umbrella repository and a submodule it contains are two
distinct topics — related, but not two spellings of one thing — and declaring an alias between
them would be a misuse of this mechanism, not a legitimate use of it: it would fold the submodule's
`list_topics` row into the umbrella's, make a query scoped to one topic silently return the other's
items, and — because this is the same table `approve`/`reject`'s identity-group check reads — hand
the umbrella's identity standing approval rights over every item the submodule's own identity
files, with no separate decision ever having been made to grant that.

This relation — one identity legitimately acting on another's behalf, without the two being the
same identity — is out of scope for this mechanism, deliberately: it is a delegation concept, and
declaring a delegation as an alias would corrupt every other place aliasing is trusted to mean "one
identity, however spelled." **Re-open trigger**: an actual observed case where one identity
genuinely needs standing authority over a distinct identity's items and no existing mechanism
covers it — evidenced concretely, not anticipated. No such case has been observed yet, and this ADR
deliberately does not invent one to fill the gap; a vague-but-honest trigger is worth more than a
precise-but-wrong example (a same-identity case like two clones of one repository, for instance,
is already covered by aliasing itself and would not belong here). At that point the right primitive
is very likely a distinct delegation concept, not a widened alias.

### MCP exclusion

`POST /aliases`, `GET /aliases`, and `DELETE /aliases` have **no MCP tool**. The precedent this
follows is `force-approve`
([architecture.md](../architecture.md)'s MCP-exposure rule): `force-approve` stays off MCP because
exposing it would let a worker grant itself (or another item) an approval it never earned by
reaching `resolved` the normal way. Declaring an alias is the same shape of risk, one level up —
instead of one item's disposition, it changes who counts as the `requester` for every item filed
under that identity, past and future, on a server with no authentication yet to limit who can call
it. That is a standing grant of approval authority across a whole class of items, decided once and
applied everywhere, which is exactly the kind of admin judgment a worker should not be able to make
for itself in the course of doing its own work — not merely "changes metadata," which `PATCH
/items/{id}`'s `requester` field also does and *is* exposed (`set_item_requester`) because
correcting one item's own field carries none of that blast radius.

### The mistargeting detector's segment-equality rule

`GET /topics/candidates?topic=` flags a topic as probably mistargeted under two conditions, both
exact: the topic is **unserved** (no registered worker's topics match it), and some *other*,
served, item-bearing topic shares its last `/`-delimited segment. There is no similarity scoring —
no edit distance, no fuzzy match.

This is not a violation of the core's rule that a topic is opaque
([principles.md](../principles.md) P-1, [architecture.md](../architecture.md) "Domain model"): the
core already knows a topic is a `/`-separated path and that segment boundaries are meaningful —
that knowledge is what prefix matching (`topic_matches`) has always relied on. Comparing one
well-defined segment against another uses only that existing knowledge; it adds nothing the core
didn't already know about the shape of a topic.

Choosing the **last** segment specifically, rather than any segment or the whole string, is a
narrower claim than "the core knows segments exist" alone justifies — it is informed by the
`org/repo` convention this codebase's topics happen to follow, where the last segment is the part
most likely to be reused verbatim across a rename or a scope change. What earns that choice a place
in the core, rather than pushing it up to the application layer, is that it stays exact rather than
fuzzy: comparing the last segment for equality is a single well-defined test with no scoring
involved, and matching against *any* segment (not just the last) would be considerably noisier —
common leaf-level names collide constantly, but a full segment match at a specific, fixed position
is a much rarer coincidence. Edit distance is a different kind of knowledge entirely — it would
compare topics as arbitrary strings with no regard for where their structure actually is. That is
why edit-distance guessing stays out of scope here rather than being an obvious next step: it isn't
a finer-grained version of the same rule, it's a different, unprincipled one.

The detector is always advisory. Two topics legitimately sharing a last segment (`acme/widget` and
`other-org/widget`) are a normal, unremarkable occurrence, so nothing here rejects, rewrites, or
auto-corrects — it only surfaces a candidate for a human or an agent to act on, via `set_item_topic`
(a one-off correction) or `put_alias` (a standing declaration that the spelling itself is valid and
recurring).

## Consequences

**Gained**: declaring that two spellings are the same identity is retroactive at zero cost — no
migration, no rewritten row, no backfill job — because every comparison resolves through the alias
table at the moment it runs rather than depending on what was written when. The open question of
how a topic survives a repository rename is answered by this same mechanism: declare the old name
an alias of the new one.

**Given up**: an alias, once declared, is load-bearing for authorization
([ADR-0019](ADR-0019-approve-reject-requester-match.md)), which is why the invariants above are
strict rather than permissive — a system that let aliases chain, repoint, or self-reference would
be a system where an approval right could shift without anyone deciding it should. The trade is a
stricter, less flexible primitive than a general graph of name relationships would be; that
strictness is deliberate, not an oversight (see the scope boundary above for the relationship this
explicitly does not attempt to model).

**Not changed**: tags remain untranslated and untouched by any of this (ADR-0021 already carved
out this exception, for the same P-1 reason). Item ids and `seq` aliases are a different mechanism
([ADR-0016](ADR-0016-item-seq-alias.md)) resolving a different kind of reference (one item, two
names for it) and are unaffected.

## Re-open trigger

See "Scope boundary" above for the one specific to that relation. More generally: evidence that a
one-hop restriction is actually blocking a legitimate case (a genuine chain of renames, not a
delegation) would be the trigger to revisit resolution depth — with a concrete instance in hand,
not before.
