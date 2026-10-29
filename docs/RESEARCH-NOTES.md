# Implementation investigations · 2026-10-29

Three independent agent reviews, requested before snapshots and additional
providers. These are findings and next steps, not claims of implemented features.

## Paper: spatiotemporal composability

[A Programming Paradigm for Spatiotemporal Composability](https://arxiv.org/abs/2608.25512)
describes dynamic component lifecycles rather than durable container recovery.
Sections 4–6 distinguish reversible internal changes from external emissions.
Useful lessons are explicit resource cleanup/dependencies and a stable broker
boundary when backing providers change. Killing a process does not undo a remote
write or a published message. Correct inverse operations remain the component
author's responsibility; the paper does not establish exactly-once distributed
execution. Its evaluation is observational and focused on one TypeScript ecosystem.

No code changes were made from the paper. Do not adopt its framework wholesale.
Source baselines, writable workspace checkpoints, saved conversation/context, and
provider continuation tokens need distinct names and recovery semantics in Control.

## Claude behind Control's harness

The minimal native integration is an Anthropic backend alongside Codex, selected
and captured with each run. Use Rust HTTP/SSE with the existing reqwest/Serde
dependencies. Control supplies its own tool schemas and executes `tool_use` through
the host broker; Claude does not need a provider-hosted MCP connector to use them.
See [Anthropic client tool use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview).

An optional unmodified Claude Code subprocess supports personal subscription use
and streaming events, including compaction boundaries. It still owns the inner
agent loop. Users must authenticate through the supported CLI flow; do not extract
OAuth tokens or share an operator's subscription across Control users. Native
multi-user service integration should use API keys. See
[CLI integration](https://code.claude.com/docs/en/headless),
[streaming events](https://code.claude.com/docs/en/agent-sdk/streaming-output),
[subscription guidance](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan),
and [credential/hosting conditions](https://code.claude.com/docs/en/legal-and-compliance).

Keep credentials in the trusted adapter. Claude's sandbox scope differs from
Codex's; our Codex-specific credential/PID canary is not portable. The TUI and web
chat should remain provider-independent. No Claude adapter is implemented yet.

## Small abstractions with concrete uses

1. Pin a run execution manifest: provider/model, source/context identities,
   capabilities, image digest, limits and opaque credential reference. Today the
   source and image are chosen at dispatch, while model/context are captured at
   enqueue; retry can therefore change execution inputs.
2. Separate provider event translation from deterministic container supervision.
   Start with a small backend enum and concrete adapters, not a plugin framework.
   Normalize usage deliberately; providers report cache accounting differently.
3. Make subagents ordinary child runs with host-created delegation, inherited
   authority ceilings, deadlines and shared budgets. A parent identifier grants
   no permission. The current serial worker would deadlock if a parent blocked
   waiting for a queued child; bounded concurrency or a waiting state must come first.

Before automatic retries, reconcile daemon-owned containers with durable attempt
identities and account for externally visible effects. Keep API keys out of
manifests, chats and tool containers; separate sandbox identity from credential
file ownership. Host operator MCP and worker MCP remain different authority surfaces.

One independent build correctness finding was fixed alongside the tools work:
source exports must include public `tools/` build inputs referenced by Cargo and
Nix. Hidden files and symlinks remain excluded. This is not a new snapshot/recovery
implementation or a change derived from the paper.
