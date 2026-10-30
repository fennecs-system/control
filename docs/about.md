# About Control

Control adds environments, tools, knowledge and review around your own model
access. It combines a Rust harness and server, an MCP surface, a Rust/WASM web
interface, and Nix-built execution environments. Each node keeps its own data,
configuration and authority.

The broader goal is an augmentable construction tool that people can run,
improve and eventually connect across independently operated nodes. See
[ARCHITECTURE.md](ARCHITECTURE.md) for that direction and [DESIGN.md](../DESIGN.md)
for the original prompts. The [roadmap](../roadmap.md) separates implemented
behavior from future work.

## Lenses, policy and personas

A lens is a pure function over explicit context snapshots. It composes document
contents, tool descriptors and instructions; it has no owner and grants no
authority. Engineering and Research compose smaller definitions from
[the built-in library](../crates/context/src/builtin-lenses.json).

The resolver expands compositions, deduplicates selections and rejects cycles,
unknown references, excessive nesting and context over 48,000 characters. Each
run stores its resolved document revisions and instructions, so later library
edits cannot change a queued run's context. The browser's WASM inspector previews
built-in definitions; the server uses the live local library.

Effective execution authority belongs at the intersection of lens selection,
node policy and actor permissions. Current brokers enforce node/actor scope;
complete lens-based execution filtering remains future work. Persona and
workflow declarations are reference schemas, not a running distributed scheduler.

## Execution and continuity

The web UI, terminal harness and Discord share persistent chats and a durable
run service. Ordinary projects start empty, then restore the last successfully
published workspace for each new turn. Filesystem continuity does not depend on
resuming a provider CLI conversation. Model context includes up to 20,000
characters of earlier exchanges; excluded exchanges remain in chat history.

There are two model execution slots by default, with one writer per chat and a
queue capped at 32 pending runs. See [execution limits](EXECUTION.md).
`/steer` adds a next-turn follow-up. `/btw` and `/spawn` create linked chats with
captured actor/model/context and separate copies of the last published task
checkpoint. They do not share a running writable workspace or inherit Control
improvement source access.

Only explicit, authorized improvement runs receive a snapshot of the configured
Control source root. Ordinary project and improvement checkpoints stay separate.
Changes never apply to the live source or installation automatically.

Cancellation remains **Stopping** until container exit is confirmed. Startup
reconciliation checks persisted node/attempt ownership, image and workspace,
then stops verified resources by inspected container ID. Unknown or mismatched
resources pause dispatch for inspection. Failed, cancelled and interrupted work
does not replace the last good checkpoint; retry is explicit.

HTTP writes release the shared state lock, and Discord ingestion runs separately
from execution. Progress is kept in memory until a lifecycle checkpoint saves it;
a crash may lose recent progress, but not an acknowledged durable task or result.
Configuration and state live outside the checkout, with a single-writer state lock.

## Containers and credentials

Worker images are assembled directly with Nix, without a distribution base.
They use immutable image pins, non-root users, a read-only root, dropped
capabilities, resource limits and temporary home directories. There is no host
execution fallback when a worker prerequisite fails.

Codex mounts a private host `auth.json` read-only. The host checks metadata
without reading its contents; sandboxed agent tools are denied access to the
credential directory and network. Every attempt first runs an empty credential
canary that checks direct and proc-alias access, PID isolation and workspace
writes. A failed probe prevents mounting the real login. Engine-specific details
are in [the sandbox notes](../hosting/worker-sandbox.md).

The Claude adapter uses the unmodified pinned CLI with built-in tools disabled.
It sees a mailbox, while its workspace commands execute in separate offline
containers without credentials or the Docker socket. Explicit command admission
prevents replay after an ambiguous acknowledgement. See [CLAUDE.md](CLAUDE.md)
for authentication and the adapter's remaining checks.

Read-only logins cannot persist token refreshes. Refresh the supported host or
container login and submit a new run if it expires. A credential proxy, hard
live-storage quotas, persisted daemon IDs at launch and broader provider fault
verification remain roadmap work.

Use model access according to your provider's terms. Making a personal
subscription available to other people may violate those terms and risk
suspension. Node permissions do not establish contractual permission.

## Design questions

Managed agents can call `control_design_ask` with 2–4 options and an optional
recommendation. The owned worker stops and saves a bounded immutable checkpoint;
only that conversation's follow-ups wait. Answering queues a continuation with
the original actor, action, model and context. A recommendation is never consent.

If stopping or checkpointing fails, Control does not offer an unsafe continuation.
A crash before the pause checkpoint commits requires inspection/retry. Deciding
when to ask is prompt-guided, not formally verified; routine sandbox edits and
checks remain autonomous.

## Artifacts and knowledge

Artifacts use immutable manifests with relative paths, sizes, content types and
hashes verified on read and restore. Publication allows up to 512 files, 2 MiB
per file and 16 MiB per snapshot. It excludes private/harness/generated files and
rejects unsafe or incomplete project checkpoints. Failed publication retains
the attempted workspace and leaves the previous checkpoint unchanged. These
publication bounds do not enforce a hard limit on live workspace writes.

Text previews escape HTML; binary files have metadata only. Unified diff previews
bound computation and output, omit private/binary/oversized content, and show
observed changes rather than a Git commit or permission to apply them. Files
and chat output never become accepted knowledge automatically.

Knowledge has Markdown documents, backlinks, unresolved links and an SVG graph.
Saving a note records a revision and detects stale edits. Claim proposals require
evidence, scope and a recheck condition; exact case-insensitive duplicates are
rejected. Accepted claims are not automatically injected into lens context.
Authority decisions are manual records and do not grant host capabilities.

## MCP and collaboration

The UI catalogue and lens library use a shared tool registry. Related MCP methods
share one card; exact protocol names and lens filters are searchable. Registration
is discovery metadata, not a grant. Tests compare advertised HTTP, stdio and
worker tool lists with that registry.

The operator's loopback HTTP MCP endpoint exposes chat, room, lens, artifact,
search and Rust tools. The external `control mcp` stdio bridge offers read-only
chat/room/lens/artifact access, plus search, Rust snippets and browser capture.
It does not expose task submission or Control administration. Worker tools derive
identity from the active attempt, not model-supplied arguments.

Room-linked chats expose scoped read/post tools. The broker checkpoints posts
before acknowledging them and deduplicates replayed nonces. Rooms show at most
50 recent updates and linked chat titles; posts do not trigger agents or grant
authority. Private and Discord chats remain unlinked by default.

Discord is a DM-only REST-polling adapter with numeric-ID bindings, confirmed
recipient/channel mappings, cursors and message deduplication. Ingress, dispatch
and tool calls recheck actor authority. Multiple user-role accounts can belong
to one person without gaining architect or website operator access.

## Current boundaries

The website binds to loopback, validates Host/Origin, and requires an operator
session for UI/API/assets. Only `/health` is public. Tailscale publication and
authenticated remote access are not configured; private networking alone would
not establish authorization.

Codex and Claude Code are connected backends. Native API and self-hosted adapters,
artifact editing, persistent personas, concurrent swarms and live federation are
not implemented. Usage records provider-reported tokens by person, model, UTC
day and attempt; missing/incomplete telemetry stays unknown and is not a bill.

The implemented federation slice is offline: scoped peer pinning/revocation,
signed lens exports, quarantined imports and inspection. Document bodies require
opt-in at both ends. There is no live peer listener or remote execution. See
[the federation specification](FEDERATION.md) and its
[author/critic review](../reviews/2026-10-28-federation.md).

Additional tool guides: [browser capture](../tools/browser/README.md) and
[standalone Rust analysis](../tools/rust-analysis/README.md). The latter is a local
launcher, not a registered MCP tool; perf/flamegraphs remain future work.
