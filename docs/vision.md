Status: v0 alignment snapshot | 2026-08-17 | updated during implementation

# Vision

## One-line definition

docket gives headless workers a durable, topic-scoped thread to coordinate through — asked by subject, not by address, and read on the worker's own schedule, not pushed in real time. A Claude Code session is just one kind of worker.

## Why now

One person runs multiple Claude Code sessions across multiple machines at once. Each session works on a different repository and moves almost like an independent worker, but the sessions don't know about each other. All coordination goes through a human's head and copy-paste, and the more sessions there are, the more that person becomes the bottleneck.

The real-world motivation is this bottleneck, experienced firsthand. Even so, the core itself is designed from the start as a general-purpose work-management engine that isn't tied to any one tool — so the core engine can be useful to other projects too. That duality is what leads to the four-layer separation in [architecture.md](architecture.md).

## Users

Developers who keep multiple Claude Code sessions running across several machines, where the sessions work on different repositories or on components that depend on each other. Single-owner, multiple-machines is the primary premise ([scope.md](scope.md)).

## Current alternative and its failure points

Current approach: **leave draft issue files behind, and let a Claude Code session in another repo read that file.** Crossing machines means a commit/push, or copying/moving the file by hand.

Three problems actually experienced with this alternative — ranked by priority: status tracking > no notifications = cross-machine delay (the latter two tied; see [goals.md](goals.md) D-06/D-07):

- **No status tracking** — there's no way to tell whether something was picked up or finished, so a human ends up checking back manually.
- **No notifications** — even when a file is created, if the other session isn't open, or has no reason to look at that directory, it may never get read.
- **Cross-machine delay** — another machine only finds out after a commit/push cycle.

## Why a topic-scoped thread — rejected alternatives

**Email model (rejected).** The recipient has to be resolved at send time. But the real request is "fix the defect in library A," not "send this to computer 1 / session A." It should be addressed by subject, not by address.

**Messenger model (rejected).** Its premise is presence. Sessions die and come back constantly, so building coordination on top of presence means coordination evaporates every time a session goes offline. More decisively, it has no completion semantics.

**Work queue / durable thread (adopted).** Pull is the answer to "we don't know who will do it." An item survives even when nobody is around, and gets picked up later by whoever shows up.

Mechanically, each item is a work-queue entry with an assignee and a pull-based lifecycle. But the shape a requester actually experiences is closer to a stateful thread than a ticket sitting in a queue: a `topic` is the thread's subject (not a person or a machine — see "Why now" below), `state` says where the thread currently stands, `tag` labels it for later retrieval, and `comment` lets the conversation continue for as long as the subject stays relevant — closing an item ends the work, not the thread; a closed item still takes new comments. Cross-topic reference — an item filed because it was discovered against another topic ("this also affects `iyulab/other-repo`") — is carried by `Item.requester`/`Item.assignee`: `requester` is who this item is for (set at creation, e.g. the discoverer), `assignee` is who's currently holding it. See [glossary.md](glossary.md) for the field shapes, and [ADR-0010](decisions/ADR-0010-item-from-to-turn.md) / [ADR-0011](decisions/ADR-0011-requester-assignee-naming.md) for how this superseded the earlier `found-in:<discoverer-topic>` tag convention and the original `from`/`to` field names.

What's deliberately not carried over from chat/email tools: no presence requirement, no push delivery, no real-time expectation. A worker reads a thread when it starts a session, when it searches for something it needs, or when it has idle capacity to look for open work — the same three moments a human would check a mailbox or an issue tracker, minus the inefficiency of waiting on someone to be online. See [goals.md](goals.md) A-1 for the async-coordination assumption this rests on.

## Scenarios

- **S1. Follow-up work from a contract change** — Session A changes a shared library's interface and posts an item to the consuming repository's topic. Whichever session owns that repo picks it up later.
- **S2. Delegating work** — Session A discovers work outside its own context and posts an item to the relevant topic.
- **S3. Immediate query** — Session B asks about implementation details in another repository. If no one owns that topic, it fails immediately (this does not persist as an item — see `question` in [glossary.md](glossary.md)).
- **S4. Cross-machine handoff** — A summary of work in progress on a desktop, plus next steps, gets posted as an item; opening a session on a laptop picks it up.
- **S5. Human intervention** — An admin notices a stalled item on the console, refines the ambiguous request, and sends it back into flow.

  > **2026-08-20 note**: `reject`/`reopen` ([ADR-0012](decisions/ADR-0012-item-reject-reopen-transitions.md))
  > cover a narrower case than this scenario — a bare, reason-carrying bounce-back callable by any
  > worker, not gated on a human or on editing the request's content. S5's "refine" (console-only,
  > edits the ambiguous request itself) remains unimplemented.
- **S6. A different runtime (extension validation)** — an incident event from `aims` becomes an item, and iyulab's own agent picks it up to investigate and resolve.

The definition of success is covered in [goals.md](goals.md).
