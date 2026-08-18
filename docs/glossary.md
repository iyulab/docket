Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Glossary

## Core vocabulary mapping

This is the discipline that keeps layer boundaries from leaking. If a term from the right-hand column shows up in core code, reject it in review.

| Core vocabulary | Application vocabulary (layer 3, etc.) |
|---|---|
| worker | session, Claude Code session |
| topic | repo, repository, channel |
| item | card, ticket, message, mail, thread |
| claim | assign, assignment |
| body | markdown, .md file |
| stream | hook |
| budget | token budget |
| tag | (none — opaque caller-defined string, not translated; see [ADR-0009](decisions/ADR-0009-tag-and-comment-vocabulary.md)) |
| comment | (none — opaque caller-defined string, not translated; see [ADR-0009](decisions/ADR-0009-tag-and-comment-vocabulary.md)) |

Concepts in the right-hand column get translated into the left-hand column at layer 3 before reaching the core. Wherever that translation happens is the layer boundary.

## Term notes

- **`topic`**: not a JMS-style topic (pub-sub, fan-out to every subscriber). It means Kafka's topic + consumer group (competing consumers, only one gets the message). docket's items are picked up by exactly one worker, which matches the latter.
- **`claim` vs `assign`**: `claim` is a worker picking up work on its own (pull). `assign` (application-layer term, "force-assign" in the admin console) is an admin triggering that on someone's behalf (push). The core primitive is `claim` alone — `assign` is the application layer's entry point for triggering that same `claim` under admin authority.
- **`state` vs `resolution`**: `state` is the workflow stage (`open/claimed/resolved/closed`); `resolution` is why it closed (`done/duplicate/wontfix/invalid`). Splitting the two follows Bugzilla/Jira practice — one fewer state, and each admin action gets its own clear meaning.
- **`task` vs `question`**: `task` is an item with a state machine (stays on the board). `question` fails immediately with no state machine (does not stay on the board). See [vision.md](vision.md) S3.
- **`found-in:<discoverer-topic>` as a cross-topic reference**: recording that an item was discovered against another topic — e.g. a downstream repo filing an item against an upstream one it depends on — is an opaque `tag` of the form `found-in:<discoverer-topic>` attached to the item at creation, the same way an `@mention` names a target in a chat tool, except the target is a `topic`, never a worker or a person. This is a caller-defined convention, not something core parses or interprets — core still never resolves "who" an item is for; it only ever indexes "what" (a `topic`) the tag names. The item's own `topic` says which topic must act on it; `found-in:` says which topic found it — together they give a **topic-to-topic** reference a consumer like `docket-console` reads. Any other `<key>:<topic>`-shaped tag is equally valid as an opaque convention (core imposes no fixed vocabulary of keys), but `found-in:` is the one with real precedent — reuse it before inventing a new one for the same need. If core ever needs to interpret this prefix itself (e.g. to answer "list items found-in topic X" without a table scan), that crosses into new core vocabulary and needs its own ADR — see [ADR-0009](decisions/ADR-0009-tag-and-comment-vocabulary.md)'s re-open trigger.
- **`found-in:` vs `Item.from`/`Item.to` — two different "from/to" axes, do not conflate**: `found-in:` (above) relates two **topics** (which topic found this vs. which topic owns it). `Item.from`/`Item.to` ([ADR-0010](decisions/ADR-0010-item-from-to-turn.md)) relate two **parties** (which requester this item is for vs. which worker currently holds it) — a worker id or human identifier, never a topic. An item can carry both at once and they answer unrelated questions; `docket-console`'s detail view shows the `found-in:` tag chip and the `from`/`to` fields side by side for exactly this reason.
