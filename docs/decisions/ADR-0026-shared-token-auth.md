Status: v0 implementation | 2026-10-29 | implemented

# ADR-0026: A single shared bearer token gates the API, opt-in

## Context

`docket-core` has had no authentication since M1. [usage.md](../usage.md) §9 said so and said to keep it
off untrusted networks "until M4"; the default bind of `127.0.0.1` ([open question
51](../usage.md)) made that safe by default, and a deployment that wants workers on other machines
overrides `DOCKET_BIND` and takes the exposure knowingly.

That arrangement stops being tenable once a server is reachable beyond a network the owner controls.
Anyone who can open the port can then read every item and comment and write anything, including
`DELETE /items/{id}`, which destroys an item with no way back, and the admin operations
(`force-close`, `redact`, `aliases`). What the owner of such a deployment needs is the smallest
mechanism that makes the port useless without a secret — not an identity system.

The premise that keeps this small is [ADR-0006](ADR-0006-single-owner-later.md): the primary target
is one person owning several machines. Every client that should be let in is the same owner's.

Three questions were open: how a client authenticates, whether the transport is encrypted, and
whether a worker may be restricted to some topics.

## Options considered and trade-offs

**How a client authenticates:**

- **Accept: one shared bearer token** (adopted). One secret, set on the server and on each client,
  sent as `Authorization: Bearer <token>`. It is the least machinery that closes the hole, it fits the
  single-owner premise, and it needs no new storage: nothing about a worker or a session is persisted.
- **Reject: a token per worker.** Its value is revoking one party without disturbing the rest, which
  is a multi-party need; with one owner it is the cost of issuing, renewing and distributing N
  secrets for no protection a single secret does not already give. It also would not make `author`
  trustworthy: `author` is a string the caller supplies, and a per-worker token proves possession of
  a credential, not that the credential's holder is the party named in a comment. The decision is
  reversible in one direction only by adding machinery later, which is why it is deferred rather
  than precluded.
- **Reject: rely on the network alone** (a firewall, a private network, or a reverse proxy's own
  login in front). That is the status quo for a trusted network and stays valid there, but as the
  *only* layer it leaves `docket-core` open on whatever interface it is bound to, and the first
  misconfigured bind or forwarded port undoes it silently. A check inside the server fails closed.

**Whether it is mandatory:**

- **Reject: always required.** It would break every existing deployment and every local run on
  upgrade, including the loopback default where the exposure the token addresses does not exist.
- **Accept: opt-in through `DOCKET_TOKEN`** (adopted). Unset keeps today's behavior. Set, it is
  enforced. The two failure modes are made loud instead of silent: a token that is set but blank or
  shorter than 16 characters **refuses to start** (a placeholder that looks like protection is worse
  than none), and starting without a token on a non-loopback address prints a warning that the API is
  open to anyone who can connect.

**Transport:**

- **Reject: TLS inside `docket-core`.** Certificates, renewal and a TLS stack are a large surface for
  a headless queue whose philosophy is to stay thin, and every deployment that needs TLS already has
  a place that terminates it.
- **Accept: terminate TLS in front** (adopted). A bearer token over plain HTTP is readable by anyone
  on the path, so a server reachable across a network you do not trust belongs behind a TLS-terminating
  reverse proxy. [usage.md](../usage.md) §9 states this next to the token.

## Decision

- **Server.** `docket-core` reads `DOCKET_TOKEN`. When set, every request to the API (at the root and
  under `/api`) must carry `Authorization: Bearer <token>`; otherwise it gets `401` with
  `WWW-Authenticate: Bearer` and a JSON error body. The comparison is constant-time. The scheme is
  matched case-insensitively.
- **What stays open.** `/status` (version and idle seconds, which the self-updater polls), and the
  console's HTML shell and static assets. They carry no item data, and the console has to load before
  it can ask for a token.
- **Clients.** `docket-mcp` and `docket-cc` send the token on every request when `DOCKET_TOKEN` is
  set in their environment. `docket-core-updater` sends it on its post-restart smoke probe — which
  reads `/items` — and **only** on that request: its HTTP client also talks to the release host, which
  must never see it. Without that, an update to a server with auth enabled would fail its health
  check and roll back every time. The console keeps the token in the browser's local storage, asks
  for it on the first `401`, and never puts it in a URL.
- **Not a launcher concern.** The launchers talk only to the release host and pass their environment
  through to the worker they start, so `DOCKET_TOKEN` reaches `docket-mcp`/`docket-cc` unchanged.

## What this does not do

- **No topic access control.** Any holder of the token may register as any worker, claim for any
  topic and read any item. Every holder is the same owner, so there is nothing to separate; this is
  the "topic access control" question from M4, answered by leaving it to the multi-user Later item.
- **No identity.** `author`, `requester` and `assignee` remain strings the caller supplies.
- **No separation of the admin operations.** `force-close`, `redact`, `aliases` and `DELETE` are open
  to every holder, as they are today to every caller. A second, admin-only token is the obvious
  extension if that stops being acceptable.
- **No revocation of one party.** Rotating the token means changing it on the server and on every
  client, then restarting.
- **No content scanning.** The token limits who can read bodies and comments; it does not look inside
  them. A secret written into an item stays in the item.

## Consequences

- **Rollout order matters.** Clients send the header whenever `DOCKET_TOKEN` is set and are inert
  without it, so the safe order is: release the clients, give each client machine the token, then set
  it on the server. Setting it on the server first makes every client without it fail with `401`.
- **A breaking change for exposed deployments, none for anyone else.** Existing runs without a token
  behave as before.
- **The token is a bearer credential.** Anyone who has a copy can act as any worker. Keep it out of
  command lines and version control; set it through the process environment.

## Re-open when

- More than one party needs to be let in, or one party must be cut off alone: per-worker tokens
  ([ADR-0006](ADR-0006-single-owner-later.md)'s Later item).
- The admin operations must be reachable by fewer parties than the ordinary ones: a second token for
  them.
- A deployment cannot terminate TLS in front: revisit transport.
