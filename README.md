# Control

A bot harness for agents, tools and knowledge, built with Rust, WASM
and Nix. Chat in a UI, in a terminal or through Discord; run Codex or Claude
in isolated containers and keep their work between turns. 

## Install

For a local NixOS machine, see the [NixOS deployment guide](docs/NIXOS.md)
for one-command setup and an optional systemd user service.

On macOS, you need **Nix with flakes, Git, a running Linux container engine, and a
Linux Nix builder** matching your Mac's architecture. See [setup](docs/setup.md)
if you need a builder or want the individual build steps.

```sh
git clone https://github.com/fennecs-system/control.git
cd control
nix build           # ./result/bin/control, with the web UI built in
nix run .#setup     # build, load and pin the worker image; initialize configuration
```

Sign in to Codex, check the installation, then start the server:

```sh
nix run .#codex -- -c 'cli_auth_credentials_store="file"' login
./result/bin/control doctor
./result/bin/control serve
```

In another terminal, run `./result/bin/control open` for the web UI, or
`./result/bin/control tui` for terminal chat. Add `--directory /path/to/project`
to request an isolated project workspace. Try a small task, then a
follow-up about the same files. Open **Artifacts** to inspect the result.

Configuration lives in `~/.config/control/`; chats and outputs live in
`~/.local/state/control/`. Setup preserves both. Repeat `nix run .#setup` to
rebuild after pulling updates. [Configuration and background services →](docs/setup.md)
To start with fresh history, stop Control and delete `state.json`, `state.log`, `history/`,
`runs/` and `artifacts/` from `~/.local/state/control/`; configuration, node
identity and pinned images stay.

The default **lite** worker includes Rust tools. For Nix, Node/npm, a headless
JDK, Python and curl inside worker containers, use `nix run .#setup -- --full`
(add `--claude` for Claude). Credential-free workspace tools and new persistent
services get a private writable Nix store/database; the pinned base and model
CLI container remain read-only. Local Nix builds work offline; remote packages
still need separate scoped network access. See [worker profiles](docs/WORKER-PROFILES.md).

## Claude and Codex

Codex is the default. To start with Claude inside Control:

```sh
nix run .#setup -- --claude
./result/bin/control claude login
```

On a fresh node, `--claude` selects Claude's `sonnet` model. Rerunning setup
preserves your saved models and selection.

[Grok Build](docs/GROK.md) is a third backend: `nix run .#setup -- --grok`, then
`control grok login`, and `/model grok:MODEL` in a chat.

[Antigravity](docs/ANTIGRAVITY.md) is a fourth: `nix run .#setup -- --antigravity`, then
`control antigravity login` (or a Gemini API key file), and
`/model antigravity:MODEL`.

Choose a saved model under **Manage → Models & providers**. **+ New model** adds
another model; clicking its row selects it for new runs.

[Local text models and compaction](docs/LOCAL-MODELS.md) describes independent
local summarizers for Codex/Claude chats, configurable history windows and the
web/TUI summary-tree inspector. It also covers bounded local text generation for
agents (`control_model_generate`) and local-model agent turns (`/model
local:NAME`, driven by Control's native tool loop). `nix run .#inference` starts
llama.cpp with Metal on Apple Silicon or in the Nix container elsewhere. A Qwen
27B setup and benchmark guide is included.

You can also use Control's tools from your existing Claude Code or Codex CLI.
With Control running, register its MCP server from the checkout:

```sh
claude mcp add --transport stdio control -- "$PWD/result/bin/control" mcp
nix run .#codex -- mcp add control -- "$PWD/result/bin/control" mcp
```

This bridge provides read-only chat, lens, room and artifact access, plus search,
Rust snippets, [solvers](docs/SOLVERS.md), browser capture and project access requests
requiring operator approval. Add `--browser` to setup for the browser image and
`--solver` for the HiGHS, Z3 and CP-SAT solver image.
See [Claude setup and authentication](docs/CLAUDE.md) for both integration paths.
Use your own model access according to your provider's terms.

For self-development, a scoped [GitHub tool](docs/GITHUB.md) can publish completed
improvements as draft pull requests using a private node credential reference.
Add `--build-environment` to setup for offline Rust/WASM checks. Workers can
[spawn N tasks or request a read-only judge](docs/DELEGATION.md) using the captured
provider/model. Team budgets and execution concurrency are configured separately.

## Commands

Run `./result/bin/control <command>`:

| Command | Purpose |
| --- | --- |
| `serve` | Start the local web and MCP server. |
| `open` / `open --link` | Open an authenticated web session, or print a one-use link for another browser. |
| `tui` | Start a fresh terminal chat; `--resume ID` opens an existing one. Tab completes `/` commands; `/help` lists commands and shortcuts. Enter sends; Ctrl+Q quits. See [terminal development](docs/TERMINAL.md). |
| `doctor` | Check local prerequisites and sandbox isolation without calling a model. |
| `config init` / `config path` | Initialize or locate private configuration. |
| `mcp` | Serve local tools to an external MCP client. |
| `discord --enable` | Listen for configured Discord DMs. [Setup →](docs/setup.md#discord) |
| `browser capture FILE [WIDTH HEIGHT]` | Capture an offline HTML preview. |
| `--help` | List commands, including Claude and offline federation helpers. |

Web, TUI and operator MCP chats accept messages up to 20,000 Unicode characters.
Discord messages and shared room posts keep their separate 2,000-character limits.

These chat commands work on the web, in the TUI and in authorized Discord DMs:

| Command | Purpose |
| --- | --- |
| `/steer direction` | Queue a direction for the next turn. |
| `/btw question` | Ask in a linked side conversation. |
| `/spawn task` | Start a linked agent conversation. |
| `/listen` / `/listen off` | Enable or stop room-triggered turns in a linked chat. |
| `/open /absolute/path` | Add an approved folder at `/workspace/<name>`; list several paths (spaces, commas or one per line) to request them together; use `--as NAME` for duplicate names. [Workspaces →](docs/WORKSPACES.md) |
| `/services` / `/services stop ID` | List or stop the running services of this DM and its team (`stop all` stops every one); `/workspace` is kept. [Persistent tools →](docs/PERSISTENT-TOOLS.md#stopping-from-a-chat) |
| `/cancel` | Stop the active or oldest queued task in this DM. |
| `/rename NAME` | Name this DM (up to 32 characters). DMs are otherwise shown by a callsign such as Bravo or Indigo. |
| `/answer QUESTION_ID answer` | Answer an agent's pending design question. |
| `/port allow ID` / `/port deny ID` | TUI port approval; add `--loopback` to allow an exact loopback target. Web requests appear inline. |
| `/improve` / `/improve off` | Enter or leave architect-only Control improvement mode. |

## Features

- **DMs and artifacts.** Persistent one-to-one conversations with an agent (DMs), live command activity.
- **Bots and runs.** See active tasks, the queue and execution history. Cancel
  work, retry an interrupted attempt, or spawn background tasks with queued completion updates.
  Approved bots keep useful sessions active; durable handoffs continue runnable
  work or wait for relevant events while their services keep running.
- **DNA.** Each gene bundles knowledge, instructions and descriptive tool context; a DM's selected genes are its DNA, inherited by the workers it spawns.
  Engineering and Research are built from smaller reusable genes. Optional
  [task autonomy and bounded investigation](docs/LENSES.md) guide recovery from
  blocked plans without changing host permissions.
- **Personas.** Named, versioned compositions of a purpose (`SOUL.md`), DNA,
  standing, a default model and opt-in behaviours: spawn a `worker`, `advisor`
  or `antibody`, or give a DM the `lead` persona ([personas](docs/PERSONAS.md)).
- **Knowledge.** Write Markdown notes, connect them with `[[links]]`, explore the
  graph and [review agent-proposed claims](docs/KNOWLEDGE-TOOLS.md).
- **Rooms.** Shared updates, durable inboxes and opt-in
  [room-triggered turns](docs/ROOM-NOTIFICATIONS.md).
  Bots can pause their own automatic wakes with `control_bot_pause`;
  use `/bot resume` in that chat to resume them.
- **Services.** Persistent Nix tool containers with separately approved,
  [scoped TCP ports](docs/PORTS.md) and [commands in their private workspace](docs/PERSISTENT-TOOLS.md).
- **Tools.** Search the catalogue by tool name, MCP name or `lens:engineering`.
  [Approved web fetch and offline Firefox capture](docs/WEB-ACCESS.md), search
  and Rust playground have dedicated views.
  MCP also offers [optimized Rust microbenchmarks](docs/PERFORMANCE.md) and
  [planning tools](docs/PLANNING-TOOLS.md): a claim-graph plan and A* path search.
- **Design decisions.** Agents can pause for a choice or your own direction,
  then continue from the saved workspace when you answer.
  Chat's Requests panel groups pending decisions, ports, folders and bot requests
  from that conversation and its agents.
- **Discord.** DM your node through a bot with explicit numeric-ID bindings.
  User and architect roles are separate; server-channel messages are ignored.
- **Usage.** Inspect provider-reported tokens by person, model, day and run.
  Missing telemetry is marked unknown.
- **Improve.** Discuss and implement Control changes in an isolated source
  snapshot. Changes stay separate from the running installation for review.

This is an early local preview. API/self-hosted adapters, live federation and
artifact editing are still planned; live workspaces do not yet have hard disk
quotas. See [about Control](docs/about.md), the [architecture](docs/ARCHITECTURE.md)
and the [roadmap](docs/SWARM-ROADMAP.md) for current boundaries and direction.

For development checks and the PR workflow, see [CONTRIBUTING.md](CONTRIBUTING.md).
