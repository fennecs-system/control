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
| `control-context` | Pure lens selection and composition, captured context, shared command and tool-policy definitions (including the GitHub tools' arguments and `/github` grammar), attempt mailboxes and working notes. No mutable application state or resource permissions. |
| `control-http` | Typed HTTP response serialization and security headers. Routing, authorization and state stay in callers. |
| `control-mcp` | MCP's JSON-RPC envelope: protocol-version negotiation, method routing, `initialize`, `tools/list` and `tools/call` handling, batches, and the reply, error and tool-result shapes. A `Tools` trait supplies one audience's list and runs its calls. No state, I/O, authorization or tools. |
| `control-ai` | Provider record decoding into normalized activity, usage and final results, plus the bounded text-call contract. No credentials, networking, containers or UI. |
| `control-core` | Borrowed scheduling inputs, validated slot capacity, pure admission/dispatch/attempt-generation predicates and typed persistent-resource transitions. `std` only; optional Serde validates identities and leases at persistence boundaries. See [persistent tools](PERSISTENT-TOOLS.md). |
| `control-effect` | `std` only. `machine` defines the `Machine` trait: events and supplied time in, stamped, durability-tagged effects out, with no clocks, I/O or locks. `sync` provides `CancelToken`, a bounded cancellable channel and the `Changes` generation counter so drivers wait instead of polling. Interpreters revalidate an effect's stamp before acting. The API follows Tokio's shapes so an async driver can replace a blocking one. Not a scheduler or event bus. |
| `control-bot` | Persistent, callback-driven bots as a pure `Supervisor` machine, a slice of the [bot-control contract](research/BOT-CONTROL.md). Bots subscribe to room messages, service events and timers; budgets bound turns; commands pause, resume and stop. Cursors drop replayed events, `TurnId`s make turn requests idempotent, and `next_deadline` lets drivers sleep. Depends only on `control-effect`. The application owns authority, persistence and queuing. |
| `control-comonad` | Comonadic history views: `Space`, `Store`, `Tree` zippers and `lens` cuts that keep the focus verbatim and coarsen with distance. Pure and `std` only. |
| `control-journal` | Append-only JSON-lines history, one file per conversation. Appends write and sync only the new line; positions never move. Implements `control_comonad::Space`. |
| `control-config` | Personal node directories, the Unix-seconds clock state records use, bounded private file reads and writes, the server address and the state file's schema. `test-isolation` keeps every test build off the operator's node. |
| `control-identity` | Actors, actions, roles, the Discord access policy, DM bindings and the authorization check. Depends only on `control-config`. |
| `control-delivery` | The delivery feed for frontends: the `Delivery` record the server appends to `history/_outbox.jsonl` (also the record history keeps), the cursor-based reader the Discord bridge follows and the statuses clients treat as delivered. |
| `control-diff` | Bounded, deterministic unified-diff snapshots and previews and their line classes, used by the server, the terminal and Discord. |
| `control-url-names` | User-facing URL names (`/dm/ID`, `/dms`, `/dna`, history pages) and the permanent redirects from earlier names. |
| `control-ingress` | The inbound message contract between the Discord bridge and the server: the `discord-inbox/` record (`Incoming`), DM chat ids and links, and the `!control` / `/improve` command grammar both apply. |
| `control-client` | The loopback MCP client (`Hive`) and typed operator operations, operator decisions, the live-update follower, chat navigation and the chat-app channel core. Talks to the server only over HTTP/MCP and node files. |
| `control-tui` | The terminal interface behind `control tui` and `control-harness`. Depends on `control-client`; owns ratatui and crossterm. |
| `control-discord` | The Discord bridge (`control discord`, `control-discord`): gateway, components, delivery and the per-DM routing. Depends on `control-client` and `control-ingress`. |
| `control-isolation` | Containers as data: `RunSpec` (image, program, user, mounts, secrets, tmpfs, limits, seccomp, network, logs, input) and the `Isolation` trait that carries out every container operation (run, create, start, kill, remove, inspect, list, exec, wait, logs, stats, top, image labels, volumes), with the `Docker` backend; engine output is parsed into typed observations by pure functions. `std`, Serde and `serde_json` only. |
| `control-agent` | The agent backend interface: the `Agent` trait and its facts, a turn's typed inputs (`Turn`, `Granted`), the host context traits (`Host`, `Events`, `Node`), the `Outcome` and `Failure` a turn returns with their status and wording, and `cli`: what the CLI backends whose only tools are Control's share (base container, isolation probe, supervised turn). Also the child-process waiter and mailbox wake those use. No application state. |
| `control-codex`, `control-claude`, `control-grok`, `control-antigravity` | One agent backend each: its containers and launch arguments, decoder wiring, probe, text call, turn and `control ID login\|check`. Depend only on `control-agent`, `control-ai`, `control-isolation`, `control-context` (and Codex on `control-effect`); only the root links them. |

The `control` library (`src/lib.rs`) keeps only build information and the
operator's permission policy, which the server alone uses. Discord follows typed
delivery events from the outbox (`crates/delivery/src/lib.rs`) and reads no
other application state except which inbound DMs were accepted. Authorization
and DM binding are rechecked at delivery.

## Isolation

`control-isolation` is the only place a container's shape is decided: callers
build a `RunSpec` and hand it to an `Isolation` handle, and `Docker` renders it
to one `docker run` argument vector in a pure function. Credentials are a
separate `secrets` field, so every secret that enters a container is typed and
listed in one place. Every container operation in the root goes through the
trait: `src/engine.rs` only hands out the node's backend, and engine output
(inspections, listings, stats) reaches callers as typed `Observed` values.
The standalone `tools/browser` and `tools/rust-analysis` binaries still run
the engine CLI themselves.

## MCP transports

Every MCP server answers through `control-mcp` and keeps its own framing,
authentication and tools. The operator endpoint (`/mcp`, `src/mcp_http.rs`)
parses a body once; GitHub, package and toolbox calls run before the state
lock, in that order, and everything else reaches the operator MCP under it.
The chat bridge (`control mcp --chat`) and the read-only bridge (`control mcp`)
share the stdio framing. The room and worker servers check a run's authority
in their own order and build their replies with `control-mcp`. Audiences do not
share tool sets or authorization, and each server keeps its own wording for
malformed requests.

## Agent backends

A backend is one way to run an agent turn: the Codex CLI, the Claude Code CLI,
the Grok Build CLI, the Antigravity CLI, or a local endpoint whose tool loop
Control runs itself. `Backend` (`src/backend.rs`) stays the closed tag
persisted with each run and attempt, and `Backend::agent()` is the only
dispatch from it. What a backend supplies is the `Agent` trait in
`control-agent`: identity and default model, where its image is pinned, how
its login is checked, the offline isolation probe, the bounded text call and
its decoder, who runs a turn's tool loop (`runs`), and the facts listed below.
`control-codex`, `control-claude`, `control-grok` and `control-antigravity`
each implement it in their own crate; the local endpoint's implementation is
still `src/local_agent.rs`. `worker::execute` prepares the workspace, renders
the prompt and dispatches once through the trait.

A CLI turn (`Turns::turn`) receives a `Turn` of typed values (attempt id,
isolation handle, node, image, model, prepared workspace, rendered prompt,
admission time, whether Control's tools are served, and the login) and a
`&mut dyn Host`: cancellation, design requests, tool pumping for its
mailbox, the attempt's time limit and handoff (`poll`), stopping its
containers, the declared observation, and an `Events` sink a reader thread
records activity and usage through. It returns an `Outcome`: not launched,
or launched with its usage and a reply or a `Failure`. `Failure` is an enum
(blocked, a classified provider `Cause` such as a usage limit or rejected
login, the CLI's own ending, unexpected tools, time limit, cancellation,
design wait, interruption, an unfinished tool call); its status and wording
live in `control-agent`, and `agent_host::finish` turns it into attempt
state. The host implements these traits in `src/agent_host.rs`; no backend
names `State`, `Attempt` or a root module.

Backends never locate credentials. The host (`src/logins.rs`) finds the one
implicit account per backend at the path its backend used to look at itself,
checks its metadata and hands it in as a `Granted` (form, path, owner, and
whether it may be written back, which the host decides); the backend only
decides where its container mounts it and how its CLI saves a refreshed
sign-in (`Keeps`). Login and
check subcommands receive the account directory the same way.

Claude, Grok and Antigravity share `control_agent::cli`: the base container,
the shell isolation probe, and one turn from probe through mailbox and launch
to the supervised reply, parameterised by each backend's data (`Cli`) and its
own container. Control's shell tool, which those probes and the host shell
use, is `src/host_shell.rs`. `tests/crate_boundaries.rs` checks that only the
root depends on a backend crate.

Still open:

- The local endpoint's crate (`control-local`). Its loop (`local_agent::run`,
  `context`, `thinking`) already works through its own `Host` trait, but its
  nudge texts share `worker_tools::nudges` with the host, its tests use the
  tool registry, personas and toolbox, and its host (`Live`, `execute`) reads
  the endpoint registry, the chat's compactor, bot continuation, the turn
  inbox, observations and recall from `State`.
- Prompt rendering (`turn_context.rs`) uses the root `identity::Workspace`;
  it stays with the host, which hands a backend the rendered prompt.
- Endpoint selection still asks whether a model is the local kind
  (`providers.rs`), as the registry it reads is that backend's.

### Differences in treatment

Backends should differ by model, not by what the harness gives them. These are
the places they differ today. Each is a named fact on `Agent` or listed here so
it can be evened out on purpose; do not add to the list.

| Difference | Codex | Claude | Grok | Antigravity | Local |
| --- | --- | --- | --- | --- | --- |
| Tool loop (`runs`) | its own CLI | its own CLI | its own CLI | its own CLI | Control's loop (`local_agent`), with its own step cap, thinking levels and in-turn compaction |
| Shell (`host_shell`) | CLI's sandboxed shell inside the turn container | Control's shell tool in a separate offline container | same as Claude | same as Claude | Control's shell tool |
| Web search (`own_web_search`) | the CLI's built-in search stays on in a turn (its text calls turn it off) | Control's search tool only: its own tools are removed | same as Claude | same as Claude | Control's search tool |
| Turn context (`own_prompt`) | captured context and policy prompt (`worker::render_prompt`) | its own shorter prompt (`control_claude::render_prompt`) unless the attempt captured its turn context | same as Codex | same as Codex | same as Codex |
| Deferred tool list | search then call through MCP | same | every Control tool through Grok's `search_tool` and `use_tool` meta-tools: it has no way to list MCP tools directly | every Control tool declared eagerly (`mcp_control_TOOL`): a lazy MCP tool is found by reading a schema file, which its file permission is denied | tools callable directly by name (`local` fact) |
| Judge source (`review_mount`) | `/review` read-only beside a writable scratch `/workspace` | `/workspace` | `/workspace` | `/workspace` | `/workspace` |
| Main container mount (`mailbox`) | the workspace | a mailbox directory beside it | same as Claude | same as Claude | no agent container |
| Isolation probe | canary-credential sandbox probe before every turn and text call | tool-isolation probe before every turn | same as Claude | same as Claude | none |
| Login check (`login`) | before the workspace is restored | inside the turn | inside the turn | inside the turn | no login |
| Default turn limit | worker and bot limits | same | same | same | its own `local-turn-timeout-secs` (3600 s) |
| Usage | reported at turn end only, so an accepted handoff waits up to 20 s for it | per model call | per model response, totals at the end | per step, totals at the end | per model call |
| Unfinished MCP call at exit | turn marked interrupted (`Failure::UnfinishedTool`) | not checked | not checked | not checked | not applicable |
| Session memory (`own_memory`) | `--ephemeral` | `--no-session-persistence` | a fresh session whose files die with the container; `--no-memory` | a fresh conversation whose files die with the container; no flag turns persistence off | Control's transcript |
| Wording | its own unclassified failure, "Worker outputs", the default provider label on a new attempt and the Usage page assume Codex | classified failures name it; its own unclassified sentence | same as Claude, or Grok's own ending | its own sign-in wording, or the CLI's own ending | generic |
| Compaction | configured summary model | same | same | same | the chat's own local model unless configured |
| Unexpected tools | not checked | stopped at start if the CLI lists a tool outside Control's | stopped if it lists or calls a tool outside the two meta-tools | stopped if it calls a tool outside Control's or starts a subagent; it always lists its built-in tools, which its permission rules deny | not applicable |
| Prompt delivery | standard input | standard input | a private file mounted read-only | a private file mounted read-only, sent as one stream-json message | request body |

## Client and server

The terminal, the loopback client and the Discord bridge are crates that do not
depend on the server: the compiler rejects any use of its `State`, routing or
rendering. They reach Control only over HTTP/MCP and the node's files. The
server binary depends on them, not the other way round: it runs `control tui`
and `control discord`, and uses the MCP client for `control mcp`, `control mcp
--chat` and `control package`.

Dependencies point one way. Formats that a writer and its readers share live in
leaf crates (`control-config`, `control-identity`, `control-context`,
`control-delivery`, `control-diff`, `control-url-names`, `control-ingress`)
that depend on no client and not on the server. Agent backend crates depend on
`control-agent` and leaf crates, never on the server, a client or each other;
only the server links them. Clients depend on leaf crates,
the server depends on leaf crates, and the binary composes the server with the
clients as subcommands. `tests/crate_boundaries.rs` checks this with `cargo
metadata`.

Some server modules still name `control-client` because the format they share
with clients has not left it yet: the channel settings page
(`channel::Settings`, `channels.json`), the permissions page and the chat MCP
tools (`operator::pending`, the reading of a request group), and the Intel
brief date (`channel::utc_date`). `operator::Kind` is used by the channel
settings too, so these move together, as one leaf crate for operator requests
and channel settings, once `operator.rs` and `channel.rs` are split from the
`Hive` calls beside them. `mcp_chat` is both the `control mcp --chat` client and
the server's `control_chat` worker tool; its `Hive` transport moves to the
entry point when the MCP stdio code settles.

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
