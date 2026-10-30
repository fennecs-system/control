# control-ai

Small provider protocol library, independent of application state, UI, credentials,
container policy and networking. Used by Control's Codex and Claude CLI adapters.

`Decoder` consumes provider JSON records and emits bounded, owned `Event` values.
`Output` distinguishes unknown usage from measured zero usage. Events move to
consumers; no cloned JSON trees or retained transcript is required. CLI JSONL uses
the shared `json_lines` reader with one reusable buffer capped at 1 MiB per record.
Malformed records and partial EOF handling remain explicit provider decisions.
Claude rejects unexpected tool configuration immediately through `RejectedTools`;
the executor must stop the process when it receives that event.

HTTP/SSE transports for OpenAI-compatible endpoints, other hosted providers or
local models can feed the same decoder contract. Framing, requests, credentials,
network policy and process cancellation belong to their respective adapters.
This extraction adds no API adapter or configured-provider UI options yet. An
agent harness (Codex/Claude Code) is distinct from a direct model endpoint; neither
selecting a model nor decoding its events grants tool authority.

Run `cargo test -p control-ai` for credential-free protocol checks. Existing
application fixtures also verify usage accounting and execution rejection paths.
