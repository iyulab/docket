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
- **`found-in:<discoverer-topic>` — legacy, superseded by `Item.from`**: before [ADR-0010](decisions/ADR-0010-item-from-to-turn.md), recording that an item was discovered against another topic — e.g. a downstream repo filing an item against an upstream one it depends on — meant attaching an opaque `tag` of the form `found-in:<discoverer-topic>` at creation, the same way an `@mention` names a target in a chat tool except the target is a `topic`, never a worker or a person. `Item.from` now covers exactly this need as a first-class field — set it at creation (`create_item`'s `from` parameter) instead of adding the tag. **New items should not add a `found-in:` tag.** Existing items that already carry one aren't retroactively stripped, but once `from` holds the same value the tag is redundant and safe to remove (see `docket-works` HISTORY for the 2026-08-18 backfill + cleanup that did exactly this for every non-closed item). Any other `<key>:<topic>`-shaped tag remains a valid opaque convention for needs `from`/`to` don't cover — this entry is specifically about the discoverer-reference use case `found-in:` was invented for, which `from` now owns.
- **`Item.to` vs an item's own `topic`**: the item's own `topic` names which topic's worker is generally responsible for it; `Item.to` (set by `claim`) names the *specific* worker currently holding it, and is `null` until someone does. `docket-console` displays `to`, falling back to `topic` when `to` is null, so "who should look at this" always has an answer — that fallback is display-only, not a core/API concept: `Item.to` itself stays honestly nullable.
