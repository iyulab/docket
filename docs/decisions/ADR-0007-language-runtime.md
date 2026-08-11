Status: v0 alignment snapshot | 2026-08-11 | settled (reflects the B-10 SPIKE result)

# ADR-0007: Implementation language and runtime

## Context

Implementation language and runtime were originally classified as an M4 (deployment) decision ([open-questions.md](../open-questions.md)), but once it became clear that actually starting to write `docket-core` (M1) requires settling the language first, the timing was moved up. The initial design discussion had already noted the target install experience: the local daemon needs low installation friction, and the server needs to be good at running continuously — since the requirements differ, they don't need to be the same language.

## Options considered and trade-offs

- **Go**: single-binary distribution, cross-platform (including Windows), easy to learn, fast iteration. Favors the top-priority quality attribute (simplicity), but that advantage disappears if the developer is already fluent in Rust.
- **Rust**: single-binary distribution (no runtime to install — fits the local daemon's "minimize installation friction" requirement even better than Go), and memory safety maps directly onto reliability (the second-priority attribute). But if the developer isn't already fluent, the learning curve and compile times work against simplicity (the top priority). **Since the developer is already proficient in Rust, this trade-off disappears.**
- **TypeScript (Node)**: since the official MCP SDK is TS-centric, it was tentatively adopted as `docket-mcp`'s safe default early on — but without having confirmed whether an official Rust SDK existed.

**B-10 SPIKE result** (investigated 2026-08-11): an official Rust MCP SDK, `rmcp` (`modelcontextprotocol/rust-sdk`), exists and is actively maintained — maintained directly by the MCP organization, 3.8k GitHub stars, implements the latest MCP spec (2026-07-28), passes the official conformance test suite 100%. It supports both server and client roles, stdio + streamable HTTP transports, and features like resources/prompts/sampling/OAuth/elicitation. It explicitly lacks SSE transport, but since the latest spec itself is moving toward streamable HTTP, this isn't treated as disqualifying. This resolves the one reason TS had been tentatively adopted (unconfirmed SDK maturity).

## Decision

**`docket-core`, `docket-cc`, and `docket-mcp` are all Rust (settled).** `docket-console` alone is web (TS/JS, framework TBD) — as a browser UI, there isn't much room for language choice there anyway.

## Consequences

**Gained**: all three layers (core/cc/mcp) ship as pure native binaries, matching the installation-friction goal noted above ("a single install command"). The toolchain unifies on one (Cargo), which effectively narrows [open-questions.md](../open-questions.md) #1 (monorepo tooling) down to a Cargo workspace — though the "dependency-checking tool" itself (see [architecture.md](../architecture.md)'s "Single repo, enforced by mechanism," item 1) still needs to be chosen.

**Given up**: the MCP ecosystem's reference material and examples still skew TS-centric, so when something's blocking, there may be less to reference than the TS SDK offers. If SSE transport turns out to be needed (e.g. a specific client that doesn't support streamable HTTP), that's the point to revisit this.

## Re-open trigger

Once SSE transport is actually needed, or a feature gap in `rmcp` is actually discovered during implementation.
