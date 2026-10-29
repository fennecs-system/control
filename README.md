# Control

A local Rust control plane for federated agent workflows. Lenses are pure functions over explicit context snapshots: they select knowledge and tool descriptors, without ownership or authority. Personas guide behavior, policy bounds authority, and workflows describe control flow.

The [architecture](docs/ARCHITECTURE.md) describes the broader goal: an augmentable
construction tool combining the harness, MCP, declarative Nix deployments,
optional model routing/local inference, and independently operated federated nodes.
It separates that direction from what is implemented today.

The [project page](docs/index.html) gives a short introduction and setup guide.
Build the normal release binary from `main` using the steps below.

## Build and start

For now, use macOS with Nix (flakes enabled), Git, and a running Linux
Docker-compatible engine. Building worker images on macOS also needs a
[Linux Nix builder](https://nixos.org/manual/nixpkgs/stable/#sec-darwin-builder).
Use a builder matching the image architecture: `aarch64-linux` for Apple Silicon,
or `x86_64-linux` for an Intel Mac. Codex is the default backend;
[Claude Code works in both directions](docs/CLAUDE.md).

```sh
git clone https://github.com/fennecs-system/control.git
cd control
nix run .#setup
```

`setup` builds the Rust/WASM UI and ordinary `target/release/control` binary,
initializes private configuration, then builds and loads the Nix Codex worker.
It selects the Linux image architecture to match the host and saves its immutable
image ID. Existing configuration, state and model choices are preserved; image
pins change only after every requested image builds and loads successfully.
It does not read login contents, sign in, call a model, or start services.

To include the Claude Code image, use `nix run .#setup -- --claude`, then follow
[Claude login](docs/CLAUDE.md#control-runs-claude-code). Add `--browser` to also
build and pin the optional [browser-capture image](tools/browser/README.md). Repeat setup to rebuild
after source changes. `nix develop` remains available for individual build/test
commands; it is not required before `nix run`.

### Linux builder on macOS

If you already have a Linux Nix builder, keep using it. Otherwise the flake
exposes the pinned Nixpkgs local VM bootstrap:

```sh
nix run .#builder
```

This is an explicit system setup step: the upstream installer requests `sudo`
to install the builder's SSH credentials. Follow the
[Nix builder instructions](https://nixos.org/manual/nixpkgs/stable/#sec-darwin-builder)
to configure the Nix daemon's trusted user, builder and SSH connection, then
restart that daemon. Keep the VM running in its terminal and run setup in another.
The builder does not provide a Docker engine. Control's setup command does not
change system configuration or silently provision a VM.

### Sign in and start

Sign in using your own Codex installation with file-based credential storage,
then start Control. The pinned Nixpkgs revision in this checkout provides Codex:

```sh
nix run .#codex -- -c 'cli_auth_credentials_store="file"' login
./target/release/control doctor
./target/release/control serve
```

In another terminal, run `./target/release/control open` for a local sign-in link,
or `./target/release/control tui` for the terminal chat. Try a small task and a
follow-up about the same files; **Artifacts** shows published outputs.

The binary embeds its web assets and can be installed somewhere stable, such as
`~/.local/bin/control`. Inject local configuration through private files in
`~/.config/control/`; there are no identities or credentials to edit in source.
`CONTROL_CONFIG_DIR=/absolute/path` selects another configuration directory.
Rooms, chats and the run ledger persist atomically under `~/.local/state/control/`.
A state-directory lock prevents competing servers. The web service never loads
Discord credentials. See [configuration](#personal-node-configuration) and
[worker authentication](#codex-container-workers) below before enabling Discord
or changing credential paths.

For the individual build steps instead of the setup helper:

```sh
nix develop
(cd web && trunk build --release --locked)
env -u CARGO_TARGET_DIR cargo build --release --locked --bin control
./target/release/control config init
docker load -i "$(nix build .#packages.aarch64-linux.worker-image --no-link --print-out-paths)"
umask 077
docker image inspect control-worker:nix --format '{{.Id}}' > "$(./target/release/control config path)/worker-image-id"
```

Use `x86_64-linux` for Intel. A Linux host can build the matching image locally.

HTTP responses release shared state before socket writes; static stylesheets
bypass that lock. Discord inbox admission runs independently of the single worker
slot. Progress updates stay in memory until a run checkpoint/result saves them;
a crash can lose recent progress, not an already committed task or result.

The UI includes lenses, rooms, a Knowledge workspace, an authority queue, a run ledger, self-improvement proposals, and a Rust/WASM tool window. Knowledge combines a document list, Markdown editor, linked-note graph, and evidence review. The previous saved draft is preserved as Scratchpad.md; it does not edit the repository's DESIGN.md. Website chat, agent tasks, and self-improvement proposals enqueue Codex container runs. Missing prerequisites produce blocked runs; there is no host fallback.

A node owns data and execution. A lens selects knowledge, tools, and instructions for a context. Effective tool access should be the intersection of lens selection, node policy, and user permissions. Lens lists describe selections; the concrete tools below execute through a separately authorized broker. Complete lens-based execution filtering remains future work.

The server requires a loopback bind address, checks Host/Origin headers, and requires an operator session for every UI/API/asset route. Run `./target/release/control open` for a single-use local sign-in link. Operator credentials stay in private node state and are never passed to workers. Only `/health` is public. Tailscale publication and remote access remain unconfigured. The launchd files in `hosting/` are templates; replace their `@…@` placeholders (paths and `@CONTROL_USER@`) before loading them as user services.

Lenses are pure functions over explicit context snapshots, without node ownership. They select knowledge and tools; resource permissions are enforced separately.

Knowledge submissions require a concise claim, evidence, scope, and recheck condition. Exact case-insensitive duplicate claims are rejected while pending or accepted; semantic deduplication is not implemented. Accepted entries remain visible in the inbox and are not automatically injected into lens context. Authority decisions are manual records, including requests routed to an unconnected judge; they never grant host capabilities. Persona/workflow declarations remain reference schemas.

In **Knowledge**, use **+ New note** to create a document and `[[Note name]]` to link it to another note. Documents, Graph, and Review share one page. The clickable SVG graph shows resolved note links; the editor also lists backlinks and unresolved links. These are local document records in node state. Saving confirms the note name/revision and time; stale edits retain the submitted text. Notes do not become accepted claims simply by being saved.

## Personal node configuration

`control config init` creates a private `~/.config/control/discord-access.json`
from the placeholder template, without overwriting an existing file.
`CONTROL_CONFIG_DIR` overrides the directory; otherwise `XDG_CONFIG_HOME/control`
takes precedence over `~/.config/control`. All overrides must be absolute.
`control config path` shows the resolved directory. The activated Discord bridge
reads `DISCORD_TOKEN` only from `discord.env` there. Keep the directory mode 0700
and both files mode 0600. Repository-local configuration and `.env` are not loaded.

For an existing node, move `.control/discord-access.json` into that directory
and move `.env` to `discord.env`, without printing its contents. Preserve the
existing bindings; the old `codex_owner_id` setting is accepted during migration
but no longer grants or restricts access. Move the remaining old `.control`
directory to `~/.local/state/control/` while both services are stopped. Never
overwrite an existing destination. `CONTROL_STATE_DIR` overrides the state root;
otherwise `XDG_STATE_HOME/control` takes precedence over the default. Paths are
absolute and independent of the directory used to launch Control.

Pinned image files (`worker-image-id`, optional `browser-image-id`) and the
default model (`model.json`) live in the configuration directory. An old model
default migrates from state on startup. Explicit source improvements require
`CONTROL_SOURCE_ROOT` or an absolute source directory in the private `source-root`
file. Ordinary tasks never inherit the launch directory as source access.

## Rust/WASM tool windows

`crates/context` contains the pure tool registry and selection logic, used by both the native inspector and compiled browser window. In `web/`, run `trunk build --release --locked` before building the server. The binary embeds the generated UI and serves it at `/tool-window/`. All UI logic is Rust; Trunk and wasm-bindgen generate browser interoperability code. The inspector makes no effectful calls.

## Codex container workers

Load the supplied worker image, or build `.#worker-image` on a Linux Nix builder. The image is assembled directly with `dockerTools.buildLayeredImage`, without a distribution base. Configure `CONTROL_WORKER_IMAGE` or the private `worker-image-id` file with a content-addressed `sha256:<64 hex digits>` image ID or `name@sha256:<64 hex digits>`.

Sign in to Codex on the host with your ChatGPT account using `codex login`. Workers mount only the existing `auth.json` file read-only at `/home/agent/.codex/auth.json`; credentials are never copied into the image/workspace or passed through environment variables. The host control plane checks file metadata without reading its contents. The source defaults to `$CODEX_HOME/auth.json` or `~/.codex/auth.json`; `CONTROL_CODEX_AUTH_FILE` can override the non-secret path. The file must be a private regular file owned by a non-root user; symlinks and group/world permissions are rejected. If necessary, configure host Codex with `cli_auth_credentials_store = "file"` before signing in. Never print, paste, or commit the file.

Each node authorizes its own bound Discord identities. Multiple accounts may
belong to one person and stay in the `user` role: they can run tasks without
Control source/editor, settings or operator-session access. This policy is
provider-neutral; future API and self-hosted adapters use the same boundary.
Codex is the default. The [Claude Code adapter](docs/CLAUDE.md) runs the unmodified
CLI in a pinned container, with workspace commands in separate offline containers.
The same guide covers Claude Code using Control as an MCP server.

**Provider terms:** configure model access in accordance with your provider's
terms. Making a personal subscription available to other people may violate
those terms and risk account suspension. Operators are responsible for their
configuration and use; this software's permissions do not establish contractual
permission. See [OpenAI's terms](https://openai.com/policies/terms-of-use/).

The container runs as the credential file's non-root UID/GID. A named Codex permission profile denies agent tools access to the entire Codex state directory and disables tool network access. Each run first tests a separate empty auth canary: direct and proc-alias reads must fail, the tool supervisor must be inside its own PID namespace, and workspace writes must succeed. Verification failure blocks the run before mounting real credentials. `./target/release/control --check-worker` exercises this path and one small authenticated model/tool task, reporting only a safe success/failure message.

The read-only mount cannot persist refreshed tokens. If authentication expires or token rotation requires a write, refresh the host Codex login and submit a new run; workers never make the credential mount writable. Account access remains subject to the plan's entitlements and limits. See the [official authentication documentation](https://learn.chatgpt.com/docs/auth). A broker that keeps provider credentials outside workers is planned in DESIGN.md.

The adapter uses a read-only container root, dropped capabilities, resource limits, ephemeral home, and a ten-minute deadline. New chats start with an empty project; later ordinary turns restore their last published workspace checkpoint. Only an explicit architect improvement receives a snapshot of the configured Control source root. Worker credentials never include Discord or operator access. Snapshots omit credentials, symlinks, Git metadata, build outputs and runtime state. Attempts stay under the node's `runs/` directory. Changes never apply to live source automatically.

An authenticated Codex run and sandboxed shell artifact were verified on this laptop on 2026-10-27 using the read-only account mount. Sandbox compatibility depends on the pinned Codex version and engine. The worker uses the [documented permission profiles](https://learn.chatgpt.com/docs/permissions), with no legacy `--sandbox` override. On this laptop, non-root bubblewrap requires Docker's proc masking disabled and a restricted seccomp profile with namespace/mount syscall allowances; all outer capabilities remain dropped, the container root stays read-only, and no privileged mode is used. See [hosting/worker-sandbox.md](hosting/worker-sandbox.md). Fine-grained harness network egress policy, independent validation, patch application, provider-thread resumption, and approval forwarding remain runtime work. Worker-reported checks are labeled as such.

## Chats and terminal harness

**Agents** shows active executions and the queue. **Runs** is the full execution
history, including tasks and improvements, with retry actions. Improvement is a
chat mode: type `/improve` to enable it (`/improve off` exits). The composer offers
command completion and highlights commands in purple. Discuss a change, refine it, and ask for an
implementation in the same conversation. Each message rechecks the original
actor's architect access. Ordinary task files and improvement source files have
separate checkpoints; changes remain isolated for review. Set the private
`source-root` path before using this mode. Old Improve-page links open chat.

The same chat commands work in the web UI, TUI and approved Discord DMs:

| Command | Behavior |
| --- | --- |
| `/cancel` | Cancel the active run, or the oldest queued run in this chat. `/cancel RUN_ID` selects one pending run. |
| `/steer direction` | Queue a follow-up for the next safe turn; it does not change a running provider request. |
| `/btw question` | Ask in a linked side conversation using a captured parent context. |
| `/spawn task` | Create a linked agent conversation with the parent's actor and captured model/context. |
| `/answer QUESTION_ID answer` | Answer a specific pending design question, including one from a child agent. |

Control-managed Codex and Claude agents can use `control_design_ask` for a
consequential architecture, scope or behavior choice. Web chat shows 2–4 options
and a recommendation; you can choose one or write your own direction. TUI and
Discord accept an option number or text. Recommendations are never selected
automatically. Routine sandbox edits, builds and tests remain autonomous;
deciding when to ask is guided by the agent prompt, not a formal design verifier.

Asking stops the owned worker and saves a bounded, immutable workspace checkpoint.
The question survives restart and blocks only that conversation's queued turns.
Answering queues a fresh continuation with the original actor, action, model and
context; it restores the paused files without granting new permissions. Other
chats can run while you decide. `/cancel` also dismisses a waiting task. If stopping
or checkpointing fails, the run stays interrupted or failed rather than offering
an unsafe continuation. A crash before the pause checkpoint commits requires
explicit inspection/retry. Completed pauses do not replace the last successful
conversation checkpoint until their continuation succeeds.

Child conversations restore their own copy of the last published task checkpoint;
they never share a running writable workspace or inherit improvement source.
Execution uses one slot and a FIFO queue capped at 32 pending runs. Side questions
and child agents wait their turn. Web and TUI offer command completion.

Cancellation remains **Stopping** until worker exit is confirmed. An in-flight
Claude workspace command can delay cancellation by its 25-second deadline.
After a restart, Control reconciles known resources against the persisted attempt,
node, image and workspace before stopping them by inspected container ID. Unknown
or mismatched resources pause dispatch for inspection. Failed or cancelled work
does not replace the previous published checkpoint; retry is explicit in Runs.
Hard workspace quotas, persisted container IDs at launch, graceful signal shutdown
and provider credential-refresh fault verification remain open.

Navigation pages are global, with one **Chat** destination and one **Lenses** menu item. The chat panel is absent from other pages. Local architects can use the header's **Discuss design** link to prepare a chat about that page; following the link sends nothing and creates no run. The topic is whitelisted context, not source-editing permission. Source changes still require an explicit authorized improvement action.

Chat has a top-left Conversations button, closed by default, opening a left-hand panel with New chat and the current selection. On narrow screens the panel overlays the chat. Completed replies omit status labels; progress and failures remain visible. The opposite side opens Artifacts.

Web chat and the TUI show file changes as unified diff previews: purple additions,
muted red removals, filenames, hunks and `+`/`-` markers. One card per attempt
updates after edits/commands and when the run finishes, relative to that attempt's
starting files. These are observed workspace changes, not a Git commit or approval
to apply them. Previews bound file size, output and computation; private paths,
binary files and oversized content are omitted. Discord replies include a short
plain monospaced preview and a chat link, with no green syntax highlighting.

**Manage** opens a secondary navigation column for Nodes, Access requests, Personas, Models & providers, Discord access, and Integrations. The section remains visible on direct links and becomes a horizontal row on narrow screens. The main sidebar scrolls its links independently when space is limited. Models & providers lists every saved backend/model pair; click a row to select the default for new runs. **+ New model** adds an entry without changing the default; **Edit** updates that entry. The list and default persist in private `model.json`. Queued runs and retries retain their backend/model. `CONTROL_CODEX_MODEL` supplies the Codex default until a model is saved. OpenAI API, native Anthropic API, and self-hosted adapters remain unconnected.

Chat uses one conversation area with lens tags in the bottom composer. **+ Lenses** reveals selectable library entries; selected tags remain visible when it closes. Sending captures the tags and message together. The borderless transcript refreshes independently while work is pending, preserving the composer draft. MCP `control.chat.create` accepts `lenses`, and `control.chat.lenses` updates an existing chat. Older single `lens` values remain readable and accepted for creation. Legacy and Discord chats without lenses stay unassigned.

Enter sends from the chat composer; Shift+Enter inserts a newline. This is Rust/WASM
behavior from the existing Trunk bundle (`cd web && trunk build --release --locked`).
Chat loads compiler-generated bootstrap code as an external module; transcript
scripts remain disabled. Without built assets, the send button remains usable.

The local [lens library](http://localhost:8080/lenses) supports creating and revising declarative definitions containing component lens IDs, tool descriptors, document IDs, and instructions. Engineering and Research are compositions of smaller lenses declared in [builtin-lenses.json](crates/context/src/builtin-lenses.json). Architecture notes starts without documents; attach the relevant notes in its editor. The pure Rust resolver expands nested compositions, deduplicates selections, and rejects cycles, unknown references/tools, excessive nesting, and context above 48,000 characters. The editor previews the resolved composition and exports definitions as JSON. Revisions are retained in local state; stale saves are rejected. Each new run stores its full resolved snapshot, including instructions and selected document contents/revisions, so later library or note edits cannot change that run's context. Tool selections remain descriptors: runtime authority is enforced separately, and lens-specific execution filtering is not yet implemented. The embedded WASM inspector remains a built-in-definition preview; the library editor and run resolver use the live local library.

New Discord DMs belong to a persistent chat. Replies include `http://localhost:8080/chat/<id>`; the link grants no access itself. An authenticated operator browser can open it using `./target/release/control open`. Continuations retain the original Discord identity and role. History is shared across Discord, web and TUI, with up to 20,000 characters of earlier exchanges in model context. Each turn starts a fresh Codex process, restoring ordinary project files from the last published checkpoint. Failed or interrupted work does not replace that checkpoint.

Run `./target/release/control tui`; it does not depend on the working directory. `control-harness` remains available as a compatibility executable; both entry points share the same Rust Ratatui client. It uses Control's MCP endpoint, loads the private local operator credential internally, and never receives provider credentials. The single transcript and grey-purple composer use the website palette. Use Ctrl+N or `/new`, Ctrl+Up/Down to switch chats, Enter to send, Alt+Enter for a newline, arrows or PageUp/PageDown to scroll, End to follow new replies, and Ctrl+Q to quit. `/chats` opens a keyboard picker; `/lenses` opens a composable picker with Space to toggle and Enter to apply. Esc returns to chat. `/use engineering evidence` composes lenses directly, `/use none` clears them, `/context` previews the next-message context, and `/chat` returns to the transcript. `--session <id>` or `/resume <id>` opens a specific conversation; `--check` verifies the connection without entering the terminal UI. The default server is `http://localhost:8080`, or the port in `CONTROL_ADDR`; `--server` currently permits only loopback HTTP origins. `CONTROL_OPERATOR_FILE` overrides the non-secret path to the local operator credential. Unchanged transcripts are not fetched again: the client checks session revisions and redraws only when needed.

The single `control` binary includes the server, TUI, Discord bridge (`control discord`), offline federation commands, WASM UI and worker seccomp profile. Execution still requires a Linux container engine and pinned Nix worker image. Startup preserves interrupted attempts and blocks dispatch while earlier containers from this node remain active; it does not automatically delete or adopt them. Live federation, persistent personas and distributed scheduling remain unimplemented. The hard bound on live workspace writes is still open; publication limits below do not enforce it.

Use **Artifacts** in a chat for its published file list and read-only text viewer.
The same service exposes `control.artifacts.list` and `control.artifact.read` over
HTTP/stdio MCP. Outputs are bounded to 512 files, 2 MiB per file and 16 MiB per
snapshot, with hashes verified on read and restore. Text previews are bounded;
HTML is escaped, and binary files have metadata only. Harness/private files,
symlinks and special files are excluded or rejected. Editing and syntax
highlighting are later work. Artifacts do not become accepted knowledge.

`POST /mcp` exposes a minimal stateless Streamable HTTP MCP interface (protocols `2025-11-25` and `2025-03-26`), protected by the same operator session and origin checks as the website. Chat tools list/create/read/send sessions and update their lenses. Room tools list/read/post updates. Lens tools are `control.lenses.list`, `control.lens.read`, and `control.lenses.resolve`. GET returns 405 because SSE is not implemented. This endpoint is for trusted local operator clients, not a capability granted to worker agents. The Rust session service is the shared policy boundary; UI and Discord never provide a claimed role. Pi is design inspiration only. Streaming, steering/cancellation, per-session provider switching, external tool registration and Codex feature parity remain future work.

Start a chat from a room to link them, or pass `room` to `control.chat.create`. The room lists linked chats; chats link back to the room. Room-linked workers receive a separate Rust stdio MCP server with only `control_room_read` and `control_room_post`, plus the [control-rooms skill](tools/room-mcp/control-rooms/SKILL.md). Codex launches it through the [documented stdio MCP configuration](https://learn.chatgpt.com/docs/extend/mcp). A bounded workspace mailbox carries requests to the host without tool networking, provider credentials, or operator cookies. The host derives the room/chat/run identity, rejects other tools and inactive runs, checkpoints posts before acknowledging them, and deduplicates replayed request nonces. Room reads return at most 50 recent updates and linked chat titles, never transcripts. Posts do not trigger agents, grant authority, or become accepted knowledge. Private and Discord chats remain unlinked by default.

## Usage

The Usage page records provider-reported input, cached-input, and output tokens for new Codex attempts, with totals and breakdowns by person, provider/requested model, UTC day, and run. Retries retain earlier attempts; each new attempt uses a fresh `-attempt-N` artifact directory and older run directories are retained. Cached tokens are a subset of input; missing telemetry and older runs are marked unknown rather than zero. Subscription cost is not estimated. Set the non-secret `CONTROL_CODEX_MODEL` to select a worker model; the default is `gpt-6.1-sol`. Accounting consumes Codex JSON events without storing raw transcripts. Provider counts are not a final bill; interrupted or failed attempts may have incomplete usage. External Locust review usage stays in its review report rather than these worker totals.

## Discord bridge

`control discord` supports direct messages from bound Discord user IDs. It is strictly DM-only: server-channel routing, reading, ingestion, replies, and jobs are disabled, including prefixed or mentioned messages. Send normal DM text to queue a task; send `hello`, `woof`, `help`, or `status` for an immediate built-in response. `/improve <objective>` and `!control improve <objective>` require architect access.

The bridge opens one-to-one DM channels only for locally bound IDs, verifies the channel type and recipient returned by Discord, and persists the confirmed mappings privately. It polls each conversation independently and replies to that same conversation. Private messages are not posted into the shared room. Recipient mappings and role policy are checked again at ingress and worker dispatch; a forged direct-message flag cannot bypass the policy. Bot/webhook messages and unexpected recipients are ignored. A plain-text task still runs in the user's isolated workspace; source access requires the architect's explicit improvement action.

DMs address the particular Control node connected to that Discord bot. Its private
`discord-access.json` configures labels, numeric IDs and `architect`/`user` roles;
each node should use its own bot credential. Labels are display text; numeric
bindings grant access. Duplicate labels/IDs and unknown actors are denied.
Bind IDs with `./target/release/control --bind-discord <label> <ID>` or the bridge's
`--bind <label> <ID>`, without loading `discord.env`. Configured users can chat
and run tasks; only architects may request Control source changes. Neither role
receives a website operator session through Discord. `channel_id` must remain
null: server access and legacy environment allowlists stay disabled.

Run `./target/release/control discord --check` to verify the bot and provision recipient channels without sending messages. Run `./target/release/control discord --enable` to listen. Either explicit flag permits only the bridge process to load `discord.env`; running with neither does not read it. The bridge suppresses credential-bearing errors and disables automatic mentions in replies. Queued runs retain their actor identity; messages cannot supply their own role. Legacy runs without actor provenance are denied and must be resubmitted.

If Discord refuses to deliver a user DM, run `./target/release/control discord --diagnose`. It reports only selected non-secret bot/application installation metadata, checks shared server membership with the architect, and supplies a zero-permission server installation link. It never emits raw API bodies or credentials. A listening bridge and a bot-side DM channel do not prove user-to-bot delivery. A shared server and compatible Discord privacy settings may still be required; installing Control there does not require moving the conversation out of DMs.

The bot needs access to the user's DM conversation; Discord privacy settings and app/server membership may affect messaging. The bridge skips historical messages on first activation, persists per-conversation cursors, deduplicates tasks by message ID, and backs off rate-limited reads. It is a REST-polling adapter, not a full Gateway or persistent-model-conversation implementation. Acknowledging a DM does not mean the worker is provisioned; missing worker authentication is reported back as a blocked run.

Server privacy has two layers: this application never requests server messages, while Discord permissions determine what the bot credential could access. A zero-permission invite still inherits permissions from @everyone and assigned roles. Use `--audit-server-permissions` to inspect effective role and channel overrides without reading any server messages. To deny server-text visibility at Discord's boundary, remove Administrator and explicitly deny View Channel for the bot member on categories/channels, including unsynced exceptions. A disabled Message Content intent alone is not an access boundary. DM access is independent of server channel visibility.

`hosting/dev.control.discord.plist` runs the bridge as a separate macOS user background service. Its service definition contains no token. The Integrations page reads a credential-free, freshness-checked status record and links to the bot's profile.

## Validation

Pull requests and `main` run Rust tests, Clippy and release/WASM builds on macOS
and Linux. Use [feature branches and pull requests](CONTRIBUTING.md) for changes.

The **Tools** page is a single catalogue. Search tool names or combine them with
`lens:engineering`; multiple `lens:` filters compose their saved selections. It
does not repeat lens previews or show a pile of membership tags.

`control.web.search` uses key-free DuckDuckGo HTML search with Bing fallback and
returns bounded titles, URLs, and snippets. Availability depends on provider
responses; challenges and format failures are surfaced rather than bypassed.
Queries go to public search providers; do not include private information.
`control.rust.run` compiles a standalone Rust 2024 program with the standard
library in a fresh Nix development container: no network, credentials or host
mounts, 768 MiB RAM, one CPU, 64 PIDs, 96 MiB temporary storage, 12-second compiler
and 3-second program deadlines, plus a 20-second host deadline and bounded logs.
The temporary filesystem permits executable output explicitly. It does not run Cargo
or download crates. Both tools have small web forms, authenticated HTTP MCP, and
local stdio MCP access. Active agents use the credential-free worker mailbox,
authorized against their original actor; room tools remain room-scoped. The
default development shell and worker image include `rg` and `fd`.

Snapshot/provider/subagent investigations are in [research notes](docs/RESEARCH-NOTES.md).
No implementation changes were derived from the reviewed paper. API-key
providers, resumable snapshots and child-run scheduling are not yet implemented.

The first independent tool container is [Rust analysis](tools/rust-analysis/README.md): Nix-built formatting/offline Clippy checks, a read-only source export, separate reports, no network or credentials, and bounded runtime. Fixture success and failure were verified in the actual Linux image. It is a local launcher today; MCP registration, external dependency packaging, and perf/flamegraphs remain future work.

Run `cargo test --locked --workspace` and `cargo clippy --locked --all-targets --workspace -- -D warnings`. Tests cover review transitions, knowledge duplicates, room isolation and escaping, pure lens behavior, image pin validation, Discord command parsing, and secret/symlink exclusion from snapshots. See [DESIGN.md](DESIGN.md) for the lightly edited prompts and filtered product direction, and [examples/engineer.nix](examples/engineer.nix) for the illustrative declaration schema.

Questions to resolve: Are agent runs pinned to a lens version? Which node is authoritative for cross-node writes? What retention rules should apply to rooms and evidence artifacts?

## Federation

The [federation specification](docs/FEDERATION.md) and [author/critic review](reviews/2026-10-28-federation.md) define selective exchange and future delegated execution. `control federation help` exposes the implemented offline slice: node identities, scoped peer pinning/revocation, signed lens exports, quarantined imports, and inspection. Document bodies require opt-in at both ends. There is no live peer listener or remote execution yet. Agents can propose workflows; deterministic code enforces authority, state, budgets, retries, and cleanup.

## Browser tools and local Codex integration

`./target/release/control browser capture preview.html 1280 800` renders an explicitly selected,
self-contained HTML file using offline Nix Firefox and saves a private PNG artifact.
`control mcp` exposes that capture tool and read-only Hive chat, lens, room and
artifact tools over stdio. `--root /absolute/preview/directory` explicitly changes
the browser input root. It does not
expose message sending, agent launch or operator credentials. See the
[browser tool setup and limits](tools/browser/README.md). The current profile uses
software rendering; GPU access and interactive/networked browsing are not enabled.
