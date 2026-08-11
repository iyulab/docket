Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Vision

## One-line definition

docket is a work-queue service for headless workers. A Claude Code session is just one kind of worker.

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

## Why a work queue — rejected alternatives

**Email model (rejected).** The recipient has to be resolved at send time. But the real request is "fix the defect in library A," not "send this to computer 1 / session A." It should be addressed by subject, not by address.

**Messenger model (rejected).** Its premise is presence. Sessions die and come back constantly, so building coordination on top of presence means coordination evaporates every time a session goes offline. More decisively, it has no completion semantics.

**Work queue / kanban (adopted).** Pull is the answer to "we don't know who will do it." An item survives even when nobody is around, and gets picked up later by whoever shows up. The board itself is a live representation of "whose court the ball is in right now."

## Scenarios

- **S1. Follow-up work from a contract change** — Session A changes a shared library's interface and posts an item to the consuming repository's topic. Whichever session owns that repo picks it up later.
- **S2. Delegating work** — Session A discovers work outside its own context and posts an item to the relevant topic.
- **S3. Immediate query** — Session B asks about implementation details in another repository. If no one owns that topic, it fails immediately (this does not persist as an item — see `question` in [glossary.md](glossary.md)).
- **S4. Cross-machine handoff** — A summary of work in progress on a desktop, plus next steps, gets posted as an item; opening a session on a laptop picks it up.
- **S5. Human intervention** — An admin notices a stalled item on the console, refines the ambiguous request, and sends it back into flow.
- **S6. A different runtime (extension validation)** — an incident event from `aims` becomes an item, and iyulab's own agent picks it up to investigate and resolve.

The definition of success is covered in [goals.md](goals.md).
