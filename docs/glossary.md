Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Glossary

## Core vocabulary mapping

This is the discipline that keeps layer boundaries from leaking. If a term from the right-hand column shows up in core code, reject it in review.

| Core vocabulary | Application vocabulary (layer 3, etc.) |
|---|---|
| worker | session, Claude Code session |
| topic | repo, repository |
| item | card, ticket, message, mail |
| claim | assign, assignment |
| body | markdown, .md file |
| stream | hook |
| budget | token budget |

Concepts in the right-hand column get translated into the left-hand column at layer 3 before reaching the core. Wherever that translation happens is the layer boundary.

## Term notes

- **`topic`**: not a JMS-style topic (pub-sub, fan-out to every subscriber). It means Kafka's topic + consumer group (competing consumers, only one gets the message). docket's items are picked up by exactly one worker, which matches the latter.
- **`claim` vs `assign`**: `claim` is a worker picking up work on its own (pull). `assign` (application-layer term, "force-assign" in §11.4) is an admin triggering that on someone's behalf (push). The core primitive is `claim` alone — `assign` is the application layer's entry point for triggering that same `claim` under admin authority.
- **`state` vs `resolution`**: `state` is the workflow stage (`open/claimed/resolved/closed`); `resolution` is why it closed (`done/duplicate/wontfix/invalid`). Splitting the two follows Bugzilla/Jira practice — one fewer state, and each admin action gets its own clear meaning.
- **`task` vs `question`**: `task` is an item with a state machine (stays on the board). `question` fails immediately with no state machine (does not stay on the board). See [vision.md](vision.md) S3.
