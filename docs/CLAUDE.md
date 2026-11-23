# Claude and Control

Claude Code can use Control's tools, and Control can run Claude Code as a worker.
The two directions use separate credentials and execution boundaries.

## Claude Code uses Control tools

Run your own Claude Code as usual, with its own login and workspace, and register
the local Control binary as a stdio MCP server:

```sh
claude mcp add --transport stdio control -- /absolute/path/to/control mcp
```

Start Control first, then reload Claude Code. The bridge uses the local operator
credential internally; never put that credential in Claude configuration. The
bridge belongs on the trusted host, outside workers.

It exposes read-only chat, lens, room, knowledge, package and artifact access,
GitHub reads, web search, Rust snippets, [solvers](SOLVERS.md), local model
generation and browser capture. It also forwards knowledge proposals, project
access requests and GitHub publication, which the server authorizes and which
need operator approval or a configured binding. It cannot submit jobs or
administer Control.

Browser capture needs the browser image (`nix run .#setup -- --browser`, or see
[the browser tool README](../tools/browser/README.md)). Pass
`--root /absolute/preview/directory` after `mcp` to choose its input directory.

To let Claude Code drive chats as the operator (create DMs, send prompts, read
progress, cancel runs, stop services; approvals only by opt-in), register the separate
[chat MCP](CHAT-MCP.md) (`control mcp --chat`).

## Control runs Claude Code

Codex remains the default backend. The Claude adapter runs the unmodified Claude
Code binary pinned by `flake.lock` in a Nix-built Linux image with the same
development tools as the Codex image. There is no Node service or custom OAuth.

With a Linux Nix builder and a running container engine:

```sh
nix run .#setup -- --claude
./result/bin/control claude check
./result/bin/control claude login
```

`check` tests offline workspace isolation without mounting a login or calling a
model. `login` runs Claude's own interactive sign-in inside the image; never paste
codes or credentials into chat. The login lives under `~/.config/control/claude/`.
Control checks its metadata and mounts only `.credentials.json`, read-only, into
task runs, so runs cannot modify it. It never extracts a macOS Keychain entry.
`CONTROL_CLAUDE_AUTH_FILE` can point to another absolute, private credential file.
Repeat `login` when the session expires.

In **Manage → Models & providers**, click a saved Claude Code model to make it
the default, or use **+ New model** / **Edit** to manage backend/model pairs. The
default applies to new runs. Existing runs and retries keep their backend and
model, and a failure never falls back to another backend or the host.

### Isolation

Claude's built-in tools, user/project settings, slash commands, skills and other
MCP servers are disabled; it receives only Control's MCP tools. The harness sees
a mailbox, not the project tree. `control_workspace_run` executes a bounded shell
command in a separate offline container with the task workspace and no
credentials, harness processes, host configuration or Docker socket. Node
permissions apply to every call. Command intent is recorded before execution, and
an ambiguous acknowledgement is not replayed.

### Usage accounting

The adapter turns Claude's JSON stream into Control's run records. Cache reads
count within total input. Each attempt records its accounting: `reported`,
`partial` (stopped before the result; a lower bound), `unknown`, or
`not_launched` (no model call). Missing usage stays unknown, never zero.
Provider-reported costs are not subscription billing. Artifacts and workspace
checkpoints use the same service as Codex, as do cancellation and container
reconciliation at startup.

Known gaps: hard storage quotas on live workspaces and wider auth-expiry fault
tests.

## Terms

Use your own model access under the provider's terms. Sharing a personal
subscription with other people may violate them; Control roles do not establish
contractual permission. Subscription runs do not use `--bare`, which skips
subscription login. See Anthropic's
[programmatic CLI](https://code.claude.com/docs/en/headless),
[CLI flags](https://code.claude.com/docs/en/cli-reference),
[authentication](https://code.claude.com/docs/en/authentication) and
[legal and compliance](https://code.claude.com/docs/en/legal-and-compliance) pages.
