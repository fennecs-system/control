# Control

A local workspace for coding agents, tools and knowledge, built with Rust, WASM
and Nix. Chat on the web, in a terminal or through Discord; run Codex or Claude
in isolated containers and keep their work between turns.

## Install

You need **macOS, Nix with flakes, Git, a running Linux container engine, and a
Linux Nix builder** matching your Mac's architecture. See [setup](docs/setup.md)
if you need a builder or want the individual build steps.

```sh
git clone https://github.com/fennecs-system/control.git
cd control
nix run .#setup
```

Sign in to Codex, check the installation, then start the server:

```sh
nix run .#codex -- -c 'cli_auth_credentials_store="file"' login
./target/release/control doctor
./target/release/control serve
```

In another terminal, run `./target/release/control open` for the web UI, or
`./target/release/control tui` for terminal chat. Add `--directory /path/to/project`
to request an isolated project workspace. Try a small task, then a
follow-up about the same files. Open **Artifacts** to inspect the result.

Configuration lives in `~/.config/control/`; chats and outputs live in
`~/.local/state/control/`. Setup preserves both. Repeat `nix run .#setup` to
rebuild after pulling updates. [Configuration and background services →](docs/setup.md)

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
./target/release/control claude login
```

On a fresh node, `--claude` selects Claude's `sonnet` model. Rerunning setup
preserves your saved models and selection.

Choose a saved model under **Manage → Models & providers**. **+ New model** adds
another model; clicking its row selects it for new runs.

You can also use Control's tools from your existing Claude Code or Codex CLI.
With Control running, register its MCP server from the checkout:

```sh
claude mcp add --transport stdio control -- "$PWD/target/release/control" mcp
nix run .#codex -- mcp add control -- "$PWD/target/release/control" mcp
```

This bridge provides read-only chat, lens, room and artifact access, plus search,
Rust snippets, browser capture and project access requests requiring operator
approval. Add `--browser` to setup for the browser image.
See [Claude setup and authentication](docs/CLAUDE.md) for both integration paths.
Use your own model access according to your provider's terms.

For self-development, a scoped [GitHub tool](docs/GITHUB.md) can publish completed
improvements as draft pull requests using a private node credential reference.
Add `--build-environment` to setup for offline Rust/WASM checks. Workers can
[spawn N tasks or request a read-only judge](docs/DELEGATION.md) using the captured
provider/model. Team budgets and execution concurrency are configured separately.

## Commands

Run `./target/release/control <command>`:

| Command | Purpose |
| --- | --- |
| `serve` | Start the local web and MCP server. |
| `open` / `open --link` | Open an authenticated web session, or print a one-use link for another browser. |
| `tui` | Start a fresh terminal chat; `--resume ID` opens an existing one. Enter sends; Ctrl+Q quits. |
| `doctor` | Check local prerequisites and sandbox isolation without calling a model. |
| `config init` / `config path` | Initialize or locate private configuration. |
| `mcp` | Serve local tools to an external MCP client. |
| `discord --enable` | Listen for configured Discord DMs. [Setup →](docs/setup.md#discord) |
| `browser capture FILE [WIDTH HEIGHT]` | Capture an offline HTML preview. |
| `--help` | List commands, including Claude and offline federation helpers. |

These chat commands work on the web, in the TUI and in authorized Discord DMs:

| Command | Purpose |
| --- | --- |
| `/steer direction` | Queue a direction for the next turn. |
| `/btw question` | Ask in a linked side conversation. |
| `/spawn task` | Start a linked agent conversation. |
| `/open /absolute/path` | Add an approved folder at `/workspace/<name>`; use `--as NAME` for duplicate names. [Workspaces →](docs/WORKSPACES.md) |
| `/cancel` | Stop the active or oldest queued task in this chat. |
| `/answer QUESTION_ID answer` | Answer an agent's pending design question. |
| `/improve` / `/improve off` | Enter or leave architect-only Control improvement mode. |

## Features

- **Chat and artifacts.** Persistent conversations, live command activity and
  purple/red diff previews. Browse published files in a read-only viewer.
- **Agents and runs.** See active tasks, the queue and execution history. Cancel
  work, retry an interrupted attempt, or spawn a nonblocking batch with paged status.
- **Lenses.** Compose knowledge, instructions and tool selections per chat.
  Engineering and Research are built from smaller reusable lenses.
- **Knowledge.** Write Markdown notes, connect them with `[[links]]`, explore the
  graph and [review agent-proposed claims](docs/KNOWLEDGE-TOOLS.md).
- **Rooms.** Shared updates, durable inboxes and an optional bounded
  [follow-up turn](docs/ROOM-NOTIFICATIONS.md).
- **Services.** Persistent Nix tool containers with separately approved,
  [scoped TCP ports](docs/PORTS.md) and [commands in their private workspace](docs/PERSISTENT-TOOLS.md).
- **Tools.** Search the catalogue by tool name, MCP name or `lens:engineering`.
  [Approved web fetch and offline Firefox capture](docs/WEB-ACCESS.md), search
  and Rust playground have dedicated views.
  MCP also offers [optimized Rust microbenchmarks](docs/PERFORMANCE.md).
- **Design decisions.** Agents can pause for a choice or your own direction,
  then continue from the saved workspace when you answer.
- **Discord.** DM your node through a bot with explicit numeric-ID bindings.
  User and architect roles are separate; server-channel messages are ignored.
- **Usage.** Inspect provider-reported tokens by person, model, day and run.
  Missing telemetry is marked unknown.
- **Improve.** Discuss and implement Control changes in an isolated source
  snapshot. Changes stay separate from the running installation for review.

This is an early local preview. API/self-hosted adapters, live federation and
artifact editing are still planned; live workspaces do not yet have hard disk
quotas. See [about Control](docs/about.md), the [architecture](docs/ARCHITECTURE.md)
and the [roadmap](roadmap.md) for current boundaries and direction.

For development checks and the PR workflow, see [CONTRIBUTING.md](CONTRIBUTING.md).
