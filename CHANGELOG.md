# Changelog

Each release has a section headed `## [X.Y.Z] - YYYY-MM-DD`. The release workflow publishes that section as the
GitHub Release notes, and the tagging step refuses a version that has no section — so a release can no longer go out
with an empty or commit-list body. Changes that need a client or an operator to act get a `### Breaking` or
`### Upgrade order` heading, so a reader following the releases sees the order without a separate notice.

Releases before 0.27.0 are described on the [GitHub Releases](https://github.com/iyulab/docket/releases) page.

## [0.31.0] - 2026-11-01

### Added

- **`channel_advisory` on `create_item`** (MCP). Filing an item in a topic that holds comments and no items — one that
  reads as a comment channel — still succeeds, but the result says so: a worker registered for that topic would see the
  item in its `mine` as unclaimed work, once per subscriber. The check reads the data (comments, no items), never the
  topic's name, and happens before the create so the new item does not count.

### Fixed

- **`list_topics` (MCP) now shows `comments`.** The core has reported a per-topic comment count since 0.30.0, but the
  MCP tool dropped it, so an agent could not tell a channel from an empty topic. The field appears when it is non-zero.
- **Event delivery on an item you commented on no longer lets through the item's creation event** when the item and its
  first comment share a millisecond. The boundary is the commenter's first comment event, ordered by sequence, not a
  timestamp. No client change is needed.

### Changed

- The channel section of `docs/usage.md` and the `create_item` tool description say not to file items in a channel
  topic: announce with `cc=<channel>` on an item filed under the topic that owns it.
- Release notes are taken from this file (see above), replacing the auto-generated commit list.

## [0.30.0] - 2026-10-30

### Comments that belong to a topic

Until now a conversation could only happen on an item, which has one requester and one holder. A subject that several workers follow and add to without any of them owning it had to be faked with copies of one notice or one long-held item. A comment can now belong to a **topic** instead of an item ([ADR-0028](https://github.com/iyulab/docket/blob/main/docs/decisions/ADR-0028-topic-comments.md)).

- **Write and read.** `add_comment` and `list_comments` take `topic` or `item_id` (exactly one). A topic comment has no requester, assignee, `turn`, state or claim, so it never appears in `mine` or in an unclaimed-work count. Over HTTP: `POST /topic-comments`, `GET /topic-comments?topic=`.
- **Following is registering.** A worker registered for a topic (a prefix, declared aliases folded) sees the topic's comments in `list_events` as events with a `topic` and no `item_id`. There is no new kind of registration.
- **Discovery.** `list_topics` lists a topic that has only comments, with a `comments` count and `count` 0.
- **An exact cursor.** Every comment, on an item or a topic, now carries a `seq`, monotonic across both and never reused; `list_comments` takes `after=<seq>` to resume exactly where a reader stopped. Comment writes acknowledge their `seq`.
- **Telling a channel about an item.** `create_item` takes an optional `cc` (a topic): the adapter then writes one line naming the new item there. Nothing is sent unless `cc` is given, and the core never links an item to a topic by itself.
- **Staying with an item you commented on.** `list_events` also returns events on an item the caller has commented on, from the moment it first did, without it being the requester, the assignee or registered for the item's topic.
- **Redaction.** `POST /topic-comments/{id}/redact` overwrites a topic comment's body, like the item-comment version (HTTP only).

**Compatibility.** The changes to existing results are additive: a `seq` on comments and their acknowledgements, and `item_id` becoming optional on the two comment tools. docket-core and docket-mcp are released separately; a new docket-mcp asked for a topic comment by an older docket-core gets an error, and an older docket-mcp works against the new docket-core.

**Upgrading the database.** The comment and event tables are rebuilt on the first start of this version. The rebuild is one transaction, and just before it the server writes a point-in-time copy next to the database, `<db>.pre-topic-comments.bak`; rolling back to an older release means putting that copy back.

### Fixes and housekeeping

- The console's topic picker lists only topics that hold items, so a topic that has only comments does not offer an always-empty view.

## [0.29.0] - 2026-10-30

### Responses sized for agents

An agent's context window is the scarce resource, so tool results now answer the question asked and no more.

### Breaking

For MCP callers (result shapes — HTTP is unchanged except where noted):

- **Write tools return an acknowledgement, not a copy of the item.** `submit_item`, `approve_item`, `reject_item`, `reopen_item`, `block_item`, `defer_item`, `archive_item` and `create_item` return `id`, `seq`, `topic`, `state`, `resolution`, `assignee`, `turn` and `state_since` (`archive_item` adds `archived_at`; `null` fields are omitted; `create_item` keeps its advisories). `add_comment` returns `id`, `item_id`, `author`, `created_at`. `claim_item` still returns the whole item; use `get_item` for the full item at any time.
- **`mine` no longer lists closed items you once held** (docket-core). It is an inbox: finished work needs nothing from you. Pass `state=closed` to ask for them. `assignee=` is unchanged.
- **`list_comments` returns `{item_id, total, comments, older?}`** instead of a bare array, with the item id stated once rather than on every comment.
- **`list_topics` and `list_identities` return `{total, topics|identities, omitted?}`** and show at most 50 rows by default; narrow with `q`. A topic row carries `prefix_only: true` when it is covered only through a broader registered prefix (the `owned_by_exact` list is no longer part of the tool result), and an empty `aliases` is left out.

### New parameters

- `list_items` / `search_items`: `updated_after` (epoch ms — only items touched after it; a comment, transition or tag change counts, so a poller reads just what moved; also an HTTP query parameter), `fields` (return only the named fields, `id` always kept), `max_chars` (stop adding rows once the JSON would pass the budget; held-back rows are reported as `omitted` and the `next_offset` to continue from; the first row is always returned).
- `list_comments`: `since`, `tail`, `max_chars` (newest-first budget; `older` says how many earlier comments were left out).
- `get_item`: `comments=N` returns the item and its newest N comments in one call (`comments`, `comment_total`).
- `list_topics` / `list_identities`: `q` (case-insensitive substring of the name or an alias) and `limit`.

### Smaller tool schema

The 25 tool definitions an agent loads in every session went from 37.2K to 27.2K characters (−27%): descriptions say what a tool does and the rules a caller can get wrong, and the long explanations live in `docs/usage.md`.

### The server does the shaping

These controls are parameters of docket-core's HTTP API, not just of the MCP tools, so every client benefits (`GET /items?fields=&max_chars=&updated_after=`, `GET /items/{id}/comments?since=&tail=&max_chars=`, `GET /topics` and `GET /identities` with `q=&limit=`). The body of a list is still a bare array and a request without these parameters returns exactly what it did before. A shaped answer says what it held back in response headers: `X-Total-Count`, `X-Omitted` (matching rows not in this answer) and `X-Next-Offset` (where to continue), and every endpoint that honours the controls sends `X-Shaped: 1`. docket-core counts rows, fields and characters, never tokens, and applies no default cap (the MCP tools set their own: 50 rows for `list_topics` / `list_identities`).

**Compatibility.** docket-mcp and docket-core are released separately. A new docket-mcp works against an older docket-core: it notices the missing `X-Shaped` marker and shapes the page itself (including `updated_after`, with a `note` in the result), and the `mine` change simply does not apply until the server is upgraded. A new docket-core works with an older docket-mcp.

### Fixes

- The MCP test harness now also clears `DOCKET_PASSWORD`, so a developer machine that has it set no longer turns authentication on for the spawned core.

## [0.28.0] - 2026-10-29

### Added

- **`DOCKET_PASSWORD`**, an optional shorter secret for people (8 characters or more). The server accepts it alongside `DOCKET_TOKEN`: setting either turns authentication on, and both open the same API. The console's prompt takes a token or a password.

## [0.27.0] - 2026-10-29

### Added

- **Optional shared bearer-token authentication** ([ADR-0026](https://github.com/iyulab/docket/blob/main/docs/decisions/ADR-0026-shared-token-auth.md)). Set `DOCKET_TOKEN` on the server and the whole API requires `Authorization: Bearer <value>`; a missing or wrong value is `401`. Off unless set. `docket-mcp` and `docket-cc` send `DOCKET_TOKEN` from their environment. The console asks for it on the first `401`.

### Upgrade order

If you turn authentication on, give every client the token **before** the server enforces it:

1. Set the same `DOCKET_TOKEN` in the environment of every machine, app and agent session that runs `docket-mcp` / `docket-cc` (for Claude Code, re-run the client setup script with the variable in the environment, or `claude mcp add … -e DOCKET_TOKEN=<value>`), then restart the session or MCP connection.
2. Clients must be 0.27.0 or newer to send the token; the launcher fetches the latest release when it starts, so a restart is enough. An older client sends no header and gets `401` once the server enforces it.
3. Then set `DOCKET_TOKEN` on the server.

Know the limits: every holder of the token is the same party (no per-client revocation, no topic access control, no separation of administrative operations); plain HTTP exposes the bearer on the wire unless TLS is terminated in front; rotating means changing the server and every client to the same new value and restarting them. Do not put the value in code, a repository, an item or a log.

### Changed

- Documentation: the event log (kinds, scope, cursor and what is not recorded) is described in `docs/usage.md`; `budget` is removed from the core vocabulary ([ADR-0027](https://github.com/iyulab/docket/blob/main/docs/decisions/ADR-0027-budget-leaves-the-core-vocabulary.md)); cross-machine routing is not planned.
- CI builds and tests on Windows and macOS, on Node.js 26.
- Dependency updates: `actions/upload-artifact` 7, five cargo crates, six console npm packages.
