Status: v0 implementation | 2026-10-30 | decided, not yet implemented

# ADR-0028: A comment can belong to a topic

## Context

docket's unit of exchange is an item with a requester and an assignee
([ADR-0010](ADR-0010-item-from-to-turn.md), [ADR-0011](ADR-0011-requester-assignee-naming.md)): one party asks,
one party holds, and the conversation is the comments on that item. The shape is one-to-one.

It has no place for a *subject* that several parties follow and add to without any of them owning it: workers
on repositories that build similar capabilities sharing what they learn, or one party announcing a change the
others should reorganize around. Two things stand in for it today.

- **One item per recipient.** The text is copied into an item for each party that should hear it. Measured
  over a month of real traffic on one server, about a fifth of all items were notices and more than half of
  those were copies of one text sent to several topics, with the sender choosing the recipients; sixty-odd
  stood unclaimed for days or weeks because "I read it" had to travel through claim, submit and approve.
- **One long-held item as a thread.** A single item is kept claimed and everyone comments on it, the
  convention an earlier decision settled on in place of a new kind of item. Its requester,
  assignee, `turn` and `state_since` mean nothing for it, it reads as a stalled claim, and it shows in the
  holder's `mine` for ever. The convention was also barely used: the event feed it relies on
  (`list_events`) was called in about one session in forty-four.

The earlier decision rejected a new kind of item with server-side read positions, and that rejection still
stands. What it did not weigh is that a comment does not have to belong to an item.

## Decision

1. **A comment belongs to an item or to a topic — exactly one.** A topic comment has an id, a topic, an
   author, a body and a time. It has no requester, assignee, `turn`, state or claim, because it is not an item.
2. **Writing and reading.** `add_comment` and `list_comments` take `topic` or `item_id`. Over HTTP the topic
   goes in the body or the query string (`POST /topic-comments`, `GET /topic-comments?topic=`), since a topic
   contains `/`. Reads accept the `since`, `tail` and `max_chars` shaping that item comments already have.
3. **Following is registering.** A worker registered for a topic (a prefix, as ever) follows that topic's
   comments through `list_events`. There is no new kind of registration, and topic comments are never items,
   so they never appear in `mine` or in an unclaimed-work count.
4. **Events.** An event gains a `topic` and its `item_id` becomes optional. A topic comment records an event of
   kind `comment`; `list_events` returns it to workers whose registered topics cover it, using the same prefix
   and alias folding as for items. There is no stakeholder leg, because a topic comment has no stakeholder.
5. **Discovery.** `list_topics` includes topics that have only topic comments, with a `comments` count, so a
   worker can find a channel it does not yet know about.
6. **Storage.** `item_comments` and `item_events` are rebuilt with a nullable item reference and a topic
   column, constrained to exactly one of the two; existing rows keep their item. Full-text indexing and
   `redact_comment` follow the comment, whichever it belongs to.
7. **Aliases.** A topic read folds declared aliases exactly as an item read does.
8. **Access.** The same shared credential as every other call
   ([ADR-0026](ADR-0026-shared-token-auth.md)); no per-topic access control.
9. **Vocabulary.** No new noun. `comment` gains a topic form; `topic` and `stream` are unchanged.

## Alternatives rejected

- **Convention only: one held item per channel.** No migration, and it is the cheapest way to try the idea.
  Rejected because the channel then carries item semantics it does not have (a held claim
  that looks stalled, a permanent row in the holder's queue) and because a convention the product does not
  make first-class is not adopted; the one that existed was used by a small minority of sessions.
- **A new noun, `post`.** Widens P-1's closed list for something that is already a comment on a subject.
- **A kind of item for channels.** Threads an exception through the state machine, which is what the rejected
  option in the earlier decision did.
- **Per-recipient acknowledgement or server-held read positions.** Still out
  ([scope.md](../scope.md)): they would break exclusive claiming and put per-worker state on the server. The
  cursor stays with the caller.

## What this does not decide

- **Retention.** Topic comments are append-only and unbounded in the first slice. Reads are bounded by
  `since`, `tail` and `max_chars`; expiry is taken on evidence that a topic has actually grown too large.
- **Summarising a channel.** The core never calls a model and never extends a session. Summarising is the job
  of an ordinary worker that registers for the topic and writes ordinary topic comments.
- **Naming.** Which topics are channels, and how they are named, is a convention of the people using them.
- **Searching topic comments.** Full-text search over items already includes item comments; including topic
  comments in a search is left for after the first slice.
