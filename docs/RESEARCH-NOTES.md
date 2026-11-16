# Research notes

Findings from earlier design reviews that still inform open work. They are not
claims about implemented behavior.

## Paper: spatiotemporal composability

[A Programming Paradigm for Spatiotemporal Composability](https://arxiv.org/abs/2608.25512)
describes dynamic component lifecycles, not durable container recovery. Sections
4–6 separate reversible internal changes from external emissions. The useful
lessons are explicit resource cleanup and dependencies, and a stable broker
boundary when backing providers change. Killing a process does not undo a remote
write or a published message; inverse operations remain the component author's
job. The paper does not establish exactly-once distributed execution, and its
evaluation is observational and limited to one TypeScript ecosystem.

Do not adopt the framework wholesale. In Control, source baselines, writable
workspace checkpoints, saved conversation context and provider sessions keep
distinct names and recovery semantics.

## A native Anthropic API backend

Claude currently runs through the unmodified Claude Code CLI
([CLAUDE.md](CLAUDE.md)), which owns its own inner agent loop and suits personal
subscriptions. A native backend would instead call the Messages API from Rust
over HTTP/SSE, send Control's own tool schemas and execute `tool_use` through the
host broker, the way local models already run through Control's native tool loop
([LOCAL-MODELS.md](LOCAL-MODELS.md)). It needs no provider-hosted MCP connector.
See [client tool use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview).

Multi-user service integration should use API keys held by the trusted adapter,
never extracted OAuth tokens or a shared personal subscription. See
[subscription guidance](https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan)
and [hosting conditions](https://code.claude.com/docs/en/legal-and-compliance).
Normalize usage deliberately: providers report cache accounting differently.

## Execution manifest

Attempts already record backend, model, worker image and project base. A complete
manifest would also pin source/context identities, capabilities, limits and an
opaque credential reference, so a retry cannot silently change its inputs. Keep
API keys out of manifests, chats and tool containers.
