Status: v0 alignment snapshot | 2026-08-11 | updated during implementation

# Coverage

A capability × case-type matrix. Only the cells required for v1 are marked; everything else is deferred to the backlog.

| Capability | Typical | Boundary | Exception/anomaly | Malicious/abuse |
|---|---|---|---|---|
| Item creation | **v1** | Not started | Not started | Not started |
| Claiming | **v1** | **v1** (concurrent-claim exclusivity) | Not started (reclaiming after a dead worker → L2) | Not started |
| State transition (resolved/closed) | **v1** | Not started | Not started | — |
| Topic matching (prefix) | **v1** | Not started (empty topic, casing, etc.) | — | — |
| Admin intervention — refine | **v1** (§11.4, "the single most important feature") | Not started | — | — |
| Admin intervention — everything else (force-assign/force-close/merge/pause) | Not started (L2~L3) | — | — | — |
| Stall detection | Not started (L3, needs the console) | — | — | — |
| Question (`ask`) | **v1** (S3 scenario) | Not started (multiple owners) | — | — |
| Prompt-injection defense | — | — | — | Not started (deferred to L2~L3, still tracked as §17 risk) |

**v1-required (6 cells)**: item creation/typical, claiming/typical+boundary, state transition/typical, topic matching/typical, refine/typical, question/typical. These six line up exactly with the completion criteria for [roadmap.md](roadmap.md) M2.

**Why injection defense was left out of v1**: public scope is settled as fully public ([architecture.md](architecture.md)), but M2~M3 is, in practice, a closed environment where only the developer's own sessions are running. With topic access control ([open-questions.md](open-questions.md) #43) still undecided, there's no scenario yet where an external worker even attaches.
