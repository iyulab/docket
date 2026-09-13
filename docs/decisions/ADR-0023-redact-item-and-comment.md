Status: v0 implementation | 2026-09-13 | implemented

# ADR-0023: `redact_item`/`redact_comment` — a narrow, HTTP-only exception to append-only comments

## Context

[ADR-0009](ADR-0009-tag-and-comment-vocabulary.md) decided comments are append-only with no
edit/delete API: "corrections are new comments, not edits to old ones." That decision holds for
ordinary corrections — it does not hold when the content itself is the problem. An item or a
comment can end up carrying content nobody meant to file permanently (personal data pasted into a
body, an identifier that shouldn't have been recorded), and once that happens, "add a follow-up
comment saying it was a mistake" does nothing — the leaked text is still sitting in the row (and,
unless the shadow index is kept in sync, still findable through `search_items` after the row is
"fixed"). The two existing ways to make a row go away are both too blunt for this: `archive_item`
only hides the item from default listings, the content is still there; `delete_item` removes the
item (and cascades to its comments) entirely, which is the right tool when nothing about the item
should survive, but is destructive overkill when the item's history is otherwise fine and only one
field or one comment needs to stop existing.

This is a Type-1 (hard-to-reverse) decision under this project's decision discipline: it amends
ADR-0009's "no update API" for comments, and once a consumer calls it in production, narrowing or
reversing the shape again means a breaking change, not a local edit. It is decided here, before
implementation, rather than left to accrete as an unexamined side effect.

## Options considered and trade-offs

- **Reject — do nothing, point callers at `delete_item`**: the immediate workaround already
  available (ADR-0013), and still the right answer when the whole item should stop existing. But it
  is strictly more destructive than the problem calls for: deleting an item over one leaked field
  throws away requester/assignee history, tags, and every *other* comment in the thread along with
  it. A caller who only wants the leak gone is pushed toward using a bigger hammer than the job
  needs, or toward leaving the leak in place because deleting the whole item feels disproportionate.
- **Reject — a general `update_item(body=...)` / comment-edit API**: strictly more flexible (typo
  fixes, not just redaction), but it reopens ADR-0009's append-only guarantee for every comment, not
  just the rare redaction case, and blurs two different intents — "this was wrong, here's the
  correction" (which the append-only comment thread already handles perfectly) and "this must stop
  existing" (which is what actually needs a new operation). Weaker audit trail too: a general edit
  API invites quoting the old value in place, which is exactly the leak this exists to close.
- **Accept — `redact_item`/`redact_comment` as narrow, named operations (adopted)**: each
  overwrites specific content with a fixed sentinel (`"[redacted]"`, never a transformation of what
  was there) and nothing else about the item or comment changes — id, author, timestamps, state,
  and the rest of the thread all survive. The audit trail is a lifecycle comment naming *which*
  field or comment was touched, never quoting the value that was there.

## Decision

Two new `Store` operations, `redact_item(id, author, redact_title)` and
`redact_comment(item_id, comment_id, author)`:

- **`redact_item`** always clears `body` (set to `NULL`) when it currently holds content; it also
  clears `title` to `"[redacted]"`, but only when the caller asks (`redact_title: true`) — `title`
  is what every list view renders, so it stays untouched by default and is only overwritten when it
  itself carries the leak. State-independent (works on a closed item) and idempotent (redacting a
  field that already reads as redacted changes nothing, and records nothing).
- **`redact_comment`** overwrites one `item_comments.body` with the same sentinel, identified by
  `(item_id, comment_id)`. Idempotent, same as above.
- **Neither ever quotes the previous value**, anywhere — not in the returned `Item`/`Comment` (the
  field is just gone), and not in the lifecycle comment that records the change. The comment says
  *what* was redacted (`"redacted: body"`, `"redacted: title, body"`, `"redacted comment <id>"`),
  never the content — quoting it would relocate the leak into the audit trail meant to close it.
- **HTTP/console-only, no MCP tool** — see
  [architecture.md's MCP-exposure rule](../architecture.md#mcp-exposure-rule). Redaction is
  irreversible against durable history in the same bucket as `remove`/`merge`/`force-close`/
  `force-approve`/`delete`, and arguably worse: unlike `delete_item`, which removes a value along
  with everything that referenced it, `redact_item`/`redact_comment` leave the item or comment in
  place with one field silently gone — closer to the shape a worker might reach for as a casual
  self-service fix, and exactly the shortcut this keeps off MCP.
- **`comments_fts` gains its first `AFTER UPDATE` trigger** (`comments_fts_au`, mirroring
  `items_fts_au`). Before this, `item_comments` was genuinely never edited (ADR-0009), so the
  shadow index only needed `AFTER INSERT`/`AFTER DELETE` triggers to stay in sync — `redact_comment`
  is the first write path that changes a comment's `body` in place, and without the new trigger, a
  redacted comment would keep matching `search_items` by its pre-redaction text, reproducing the
  exact leak this operation exists to close.

## Consequences

**Gained**: a way to remove specifically-leaked content without destroying an item's or a thread's
otherwise-legitimate history, with an audit trail that never itself becomes a second leak surface,
and a shadow-index bug (the missing `comments_fts` update trigger) that was latent since ADR-0009
and would have resurfaced with any future comment-editing feature is closed now, ahead of that.

**Given up**: `item_comments` is no longer literally never-edited — ADR-0009's guarantee narrows
from "append-only, full stop" to "append-only except a caller-invoked, content-blind redaction."
The distinction that keeps this narrow: redaction can only ever produce the fixed sentinel, never
arbitrary new content: there is still no way to *edit* a comment's substance, only to blank it.

## Re-open trigger

If a future need shows up for editing a comment's *substance* (not blanking it) — a typo fix, a
correction that should replace rather than append — that is not covered by this ADR and does not
extend it. It goes through the same discipline that gated this one: named as its own operation,
scoped narrowly, with its own audit-trail design, not a widening of `redact_comment` into a general
update API.
