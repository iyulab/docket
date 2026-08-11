Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Open Questions

Things to decide during implementation. Future sessions must not casually settle an item on this list on their own ([AGENTS.md](../AGENTS.md)). Once an item is resolved, it's removed from here and moves to [architecture.md](architecture.md) / [decisions/](decisions/).

The timing tags are a default inferred from the roadmap structure, since the original planning document didn't specify explicit triggers — adjust as needed once work actually starts.

## M1 (core) timing — item, lifecycle, and safeguard values

1. Monorepo tooling — the package manager/workspace question has narrowed to Cargo workspace ([ADR-0007](decisions/ADR-0007-language-runtime.md)), **only the dependency-direction-checking tool is still undecided**
2. Topic separator and its valid character set
3. Whether to support tag/wildcard mid-path matching beyond prefix matching
4. Max length for `title`
5. Max size for `body` and how overflow is handled
6. How far the `refs` schema extends
7. Rules for custom-field extension
8. Whether comments are a separate entity or appended to the body
9. What happens when the requester doesn't respond while an item is `resolved` (the requester's session may already be dead)
10. Retention period and cleanup policy for `closed` items
11. Claim expiry time
12. Concurrent-claim cap per worker (WIP limit)
13. When a dead worker's claim becomes reclaimable
14. Stall-detection threshold ("no owner" vs. "no update"), and whether it's configurable per topic — ties into the observations from [B-04](backlog.md)
15. Max item depth
16. Max comments per item
17. Max consecutive extensions per session — ties into the validation in [B-05](backlog.md)
18. Max item-creation rate per worker per hour
19. Max open items per topic
20. What happens when a safeguard cap is exceeded (silently reject / notify / escalate to an admin queue)
21. API versioning policy

## M2 (mcp/cc) timing — file representation, triggers, MCP surface

22. Standard namespace for non-repo topics (`org/`, `env/`, `host/`, etc.)
23. How topics get migrated when a repo is renamed
24. Whether `question` lives in the core or only at layer 3
25. Max wait time for a question's answer
26. How to pick which owner a question goes to when there are multiple
27. Where the file representation's root lives (inside vs. outside the repo)
28. How far to push things down into `open/`
29. How to prevent a partial read while a file is being written
30. Whether to allow editing a file to change state — this is in tension with P-2 (files are not the source of truth); leaning toward allowing it would require revisiting the principle
31. Max number of items surfaced in a notification
32. Notification message format
33. Whether to notify about unclaimed items too, or only claimed ones
34. How strongly worded the injected instruction should be on an extension
35. Cap on the number of MCP tools
36. Wording for each MCP tool's description
37. Whether `decline` (refusing a request) is a separate tool, or a reason passed to `release`
38. Real-time stream protocol (WebSocket / SSE / long polling)

## M3 (console) timing

39. Default board column layout (by state vs. by topic)
40. Concurrency handling when an admin edits an in-progress item — the original planning document proposed "push a change notification to the owning worker" as a recommended approach, but this hasn't been confirmed. Rationale: since admin intervention already implies something went wrong, halting the in-progress work is arguably the right outcome anyway

## M4 (safeguards/deployment) timing

41. How to enforce treating the body as data (delimiters, system-level instructions, etc.) — [coverage.md](coverage.md) has already settled that this is excluded from v1; the mechanism itself is still undecided
42. Auth approach and per-worker token issuance/renewal cadence
43. Transport-layer security (private-network assumption vs. mandatory auth)
44. Topic access control (can any worker self-report ownership of any topic?)
45. Distribution channel (crates.io / npm / a single executable)
46. Server deployment form (whether to provide a Docker image)
47. Cross-platform scope (Windows support directly affects filename/path rules)
48. How connection info is displayed
49. Detailing completion criteria per M3/M4 milestone — M1/M2 are already concretely specified via [roadmap.md](roadmap.md)/[quality-ramp.md](quality-ramp.md), but M3 (console) and M4 (safeguards) are still abstract

## Items added — 2026-08-11, from backlog-discover

Items 1~49 above came out of the v0 interview session. The two below were newly discovered later, in a `/iyu:backlog-discover` run (premortem). Numbering continues (so existing cross-references don't shift), tagged separately by timing.

50. **[M4 timing]** Core server uptime location and failure recovery — which machine hosts the core server on an ongoing basis? How does coordination recover if that machine goes down or reboots? This is where the single-instance SQLite premise ([ADR-0004](decisions/ADR-0004-sqlite-storage.md)) meets the "multiple machines participate in coordination" premise ([vision.md](vision.md)). Decide together with the distribution channel/form (#45~46). → [B-11](backlog.md)
51. **[M1 timing, provisional until M4]** Default HTTP API binding during the pre-auth window — until auth (#42~43) is in place, should the core HTTP API default to localhost/private-network only, and what should the default be? The rationale for excluding prompt-injection defense from v1 ([coverage.md](coverage.md)) rests on a "closed environment" assumption, so from the moment M1 actually stands up that API, the code arguably needs to minimally reinforce that assumption. → [B-12](backlog.md)

## Resolved items (for reference, no longer open)

- ~~Success criteria~~ → [goals.md](goals.md)
- ~~Public scope~~ → [ADR-0005](decisions/ADR-0005-public-scope.md)
- ~~Whether multi-worker collaborative claims are allowed~~ → settled as not allowed, [architecture.md](architecture.md)
- ~~Storage engine~~ → [ADR-0004](decisions/ADR-0004-sqlite-storage.md)
- ~~Implementation language and runtime~~ → core+cc settled, mcp was tentative, [ADR-0007](decisions/ADR-0007-language-runtime.md)
