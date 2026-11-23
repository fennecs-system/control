# Library boundaries

Keep Control understandable through small interfaces and independently tested
libraries. Extract a responsibility when the boundary removes coupling or
repeated work; moving files alone does not create a boundary.

The design constraint is a small core vocabulary with decisions separate from
execution. Prefer a tool, strategy or configuration over a new core concept:
cron interpretation, recipes, personas, goal reasoning, learning procedures and
compaction strategies compose existing operations and stay out of the
supervisor's lifecycle types. Application operations enforce authorization and
durable acceptance; adapters perform I/O. A core API change must name the
generic invariant that existing interfaces cannot express.

## Workspace crates

| Crate | Responsibility |
| --- | --- |
| `control-context` | Pure lens selection and composition, captured context, shared command and tool-policy definitions, attempt mailboxes and working notes. No mutable application state or resource permissions. |
| `control-http` | Typed HTTP response serialization and security headers. Routing, authorization and state stay in callers. |
| `control-ai` | Provider record decoding into normalized activity, usage and final results, plus the bounded text-call contract. No credentials, networking, containers or UI. |
| `control-core` | Borrowed scheduling inputs, validated slot capacity, pure admission/dispatch/attempt-generation predicates and typed persistent-resource transitions. `std` only; optional Serde validates identities and leases at persistence boundaries. See [persistent tools](PERSISTENT-TOOLS.md). |
| `control-effect` | `std` only. `machine` defines the `Machine` trait: events and supplied time in, stamped, durability-tagged effects out, with no clocks, I/O or locks. `sync` provides `CancelToken`, a bounded cancellable channel and the `Changes` generation counter so drivers wait instead of polling. Interpreters revalidate an effect's stamp before acting. The API follows Tokio's shapes so an async driver can replace a blocking one. Not a scheduler or event bus. |
| `control-bot` | Persistent, callback-driven bots as a pure `Supervisor` machine, a slice of the [bot-control contract](research/BOT-CONTROL.md). Bots subscribe to room messages, service events and timers; budgets bound turns; commands pause, resume and stop. Cursors drop replayed events, `TurnId`s make turn requests idempotent, and `next_deadline` lets drivers sleep. Depends only on `control-effect`. The application owns authority, persistence and queuing. |
| `control-comonad` | Comonadic history views: `Space`, `Store`, `Tree` zippers and `lens` cuts that keep the focus verbatim and coarsen with distance. Pure and `std` only. |
| `control-journal` | Append-only JSON-lines history, one file per conversation. Appends write and sync only the new line; positions never move. Implements `control_comonad::Space`. |
| `control-config` | Personal node directories, bounded private file reads and writes, the server address and the state file's schema. `test-isolation` keeps every test build off the operator's node. |
| `control-identity` | Actors, actions, roles, the Discord access policy, DM bindings and the authorization check. Depends only on `control-config`. |
| `control-client` | The loopback MCP client (`Hive`) and typed operator operations, operator decisions, the live-update follower, chat navigation, GitHub commands and the chat-app channel core. Also the contracts the server writes or answers: the outbox delivery feed, user-facing URL names and diff previews. Talks to the server only over HTTP/MCP and node files. |
| `control-tui` | The terminal interface behind `control tui` and `control-harness`. Depends on `control-client`; owns ratatui and crossterm. |
| `control-discord` | The Discord bridge (`control discord`, `control-discord`): gateway, components, delivery and the per-DM routing. Depends on `control-client`. |

The `control` library (`src/lib.rs`) keeps only build information and the
operator's permission policy, which the server alone uses. Discord follows typed
delivery events from the outbox (`crates/client/src/outbox.rs`) and reads no
other application state except which inbound DMs were accepted. Authorization
and DM binding are rechecked at delivery.

## Agent backends

A backend is one way to run an agent turn: the Codex CLI, the Claude Code CLI,
the Grok Build CLI, the Antigravity CLI, or a local endpoint whose tool loop Control runs itself. `Backend`
(`src/backend.rs`) stays the closed tag persisted with each run and attempt.
What a backend supplies is the `Agent` trait beside it: identity and default
model, pinned image, credential readiness, the offline isolation probe, the
bounded text call and its decoder, and the names it keeps private in a
workspace. `Backend::agent()` is the only dispatch; `src/codex.rs`,
`src/claude.rs`, `src/grok.rs`, `src/antigravity.rs` and `src/local_agent.rs` each implement it. `src/worker.rs`
holds generic execution only.

The trait's signatures name only `std` and `control_ai`, so each implementation
can move to its own crate without a cycle: `control-codex`, `control-claude`,
`control-grok`, `control-antigravity`, `control-local`. A backend gets
what it needs from the host as arguments and never names application state.
This is one trait and plain crates, not a plugin runtime: backends are linked
into the binary and adding one adds a variant.

Still open before the crates can move:

- Running a turn. Claude, Grok and Antigravity share one supervisor (`claude::supervise`),
  its container flags (`claude::base`) and the host shell, which belong in a
  module of their own before any of them moves. `worker::execute` matches on the backend and each `execute`
  takes `&mut Attempt` and the shared state handle for cancellation, tool
  pumping, activity records and attempt control. Those need a narrow host
  context first.
- Prompt rendering (`turn_context.rs`) uses the root `identity::Workspace`.
- The shared failure classifier and its wording (`worker::worker_failure`).
- Checks that ask "is this the local endpoint kind" (endpoint registry,
  compaction model, capacity, the local turn limit, the tool list's `local`
  fact) compare the tag directly.

### Differences in treatment

Backends should differ by model, not by what the harness gives them. These are
the places they differ today. Each is a named fact on `Agent` or listed here so
it can be evened out on purpose; do not add to the list.

| Difference | Codex | Claude | Grok | Antigravity | Local |
| --- | --- | --- | --- | --- | --- |
| Tool loop | its own CLI | its own CLI | its own CLI | its own CLI | Control's loop (`local_agent`), with its own step cap, thinking levels and in-turn compaction |
| Shell (`host_shell`) | CLI's sandboxed shell inside the turn container | Control's shell tool in a separate offline container | same as Claude | same as Claude | Control's shell tool |
| Turn context | captured context and policy prompt (`worker::render_prompt`) | its own shorter prompt (`claude::render_prompt`), no captured turn context | same as Codex | same as Codex | same as Codex |
| Deferred tool list | search then call through MCP | same | every Control tool through Grok's `search_tool` and `use_tool` meta-tools: it has no way to list MCP tools directly | every Control tool declared eagerly (`mcp_control_TOOL`): a lazy MCP tool is found by reading a schema file, which its file permission is denied | tools callable directly by name (`local` fact) |
| Judge source (`review_mount`) | `/review` read-only beside a writable scratch `/workspace` | `/workspace` | `/workspace` | `/workspace` | `/workspace` |
| Main container mount (`mailbox`) | the workspace | a mailbox directory beside it | same as Claude | same as Claude | no agent container |
| Isolation probe | canary-credential sandbox probe before every turn and text call | tool-isolation probe before every turn | same as Claude | same as Claude | none |
| Login check | before the workspace is restored | inside the turn | inside the turn | inside the turn | no login |
| Default turn limit | worker and bot limits | same | same | same | its own `local-turn-timeout-secs` (3600 s) |
| Usage | reported at turn end only, so an accepted handoff waits up to 20 s for it | per model call | per model response, totals at the end | per step, totals at the end | per model call |
| Unfinished MCP call at exit | turn marked interrupted (`finish_codex_tools`) | not checked | not checked | not checked | not applicable |
| Session memory (`own_memory`) | `--ephemeral` | `--no-session-persistence` | a fresh session whose files die with the container; `--no-memory` | a fresh conversation whose files die with the container; no flag turns persistence off | Control's transcript |
| Wording | failure text, the default provider label on a new attempt and the Usage page assume Codex | failure text is Codex's with the name replaced | failure text is Codex's with the name replaced, or Grok's own ending | failure text is Codex's with the name replaced, its own sign-in wording, or the CLI's own ending | generic |
| Compaction | configured summary model | same | same | same | the chat's own local model unless configured |
| Unexpected tools | not checked | stopped at start if the CLI lists a tool outside Control's | stopped if it lists or calls a tool outside the two meta-tools | stopped if it calls a tool outside Control's or starts a subagent; it always lists its built-in tools, which its permission rules deny | not applicable |
| Prompt delivery | standard input | standard input | a private file mounted read-only | a private file mounted read-only, sent as one stream-json message | request body |

## Client and server

The terminal, the loopback client and the Discord bridge are crates that do not
depend on the server: the compiler rejects any use of its `State`, routing or
rendering. They reach Control only over HTTP/MCP and the node's files. The
server binary depends on them, not the other way round: it runs `control tui`
and `control discord`, uses the MCP client for `control mcp` and package
imports, and reads Discord's inbound message type and the channel settings.

The `tui` feature is on by default and is the optional dependency on
`control-tui`; `--no-default-features` builds a server without terminal code or
its dependencies. The terminal calls typed client operations for port decisions
and project approval; only `control-client` owns their HTTP routes, forms,
origin headers and confirmation rules. The Rust/WASM package under `web/` is the
browser client. Server state, routing and rendering remain coupled in places.

File formats still couple Discord to the server. The bridge reads
`discord_seen` from `state.json` and its patch log through
`control_config::read_state_in`; it writes `discord-inbox/` and
`discord-status.json`, which the server reads, and reads the server's
`discord-outbox/` notices and outbox feed. These are data contracts between
processes, not shared code.

Possible later splits:

- `control-web` for server-side HTTP/UI adapters, once web routing and rendering
  call explicit projections and authorization. `control-web-ui` could rename the
  existing browser client.
- `control-mcp` for JSON-RPC negotiation, tool discovery schemas and protocol
  errors over the existing tool catalogue. HTTP and stdio can share its protocol
  logic while keeping their own framing and authentication.

Extract each one only once its application interface is real, rather than
exporting the whole state to a cosmetic crate. These crates can still run in one
binary and call each other directly, with no extra processes, network hops,
message bus or repeated serialization.

## Application interface

Expose typed commands and bounded queries: create, send and steer a
conversation, request a project, cancel or inspect a run, and read conversation
updates. Web, MCP and terminal clients invoke the same authorization and durable
operations. Keep render-only work outside the state lock where possible: borrow
within a request and pass bounded owned snapshots across threads. Do not export
`Arc<Mutex<State>>` as a public API. See
[state and effect boundaries](STATE-BOUNDARIES.md) for the operations already
migrated.

`control-core` holds durable run transitions, admission, resource ownership and
lifecycle rules. It is not a home for provider protocols, web rendering, Discord
delivery or tool implementations. Process and container executors interpret its
effects. Provider selection and normalized events do not grant filesystem,
network or tool access. Model APIs and local endpoints use their own adapters
rather than pretending to be a CLI harness.

## Rules for new extractions

- Extract incrementally, with persistence and authorization regressions.
- Avoid an umbrella plugin runtime, cyclic dependencies and a crate per file.
  One crate per agent backend behind the `Agent` trait is the intended split.
- Model attempts attach to resources; they do not own a resource's lifetime or
  get write access to another attempt's checkpoint.
- Extract a shared executor only when real shared behavior supports it.
- Measure targeted rebuild time, render time, lock-held work and peak memory on
  fixed fixtures before and after. A crate split alone does not add scheduling
  capacity or remote-node guarantees.
