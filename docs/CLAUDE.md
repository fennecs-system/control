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
model, then mounts a fake credential in a turn container exactly as a turn
mounts the login and prints one PASS or FAIL line per reader: the CLI uid can
read it; another uid cannot, directly or through `/proc/1/root`, and it is not
in the environment. Run it after any image update. `login` runs Claude's own interactive sign-in inside the image; never paste
codes or credentials into chat. The login lives under `~/.config/control/claude/`.
Control checks its metadata and mounts that folder, writable, at
`/home/agent/.claude-login` (`CLAUDE_SECURESTORAGE_CONFIG_DIR`), so a token Claude
refreshes is kept; only the sign-in, its lock and Claude's in-flight copies stay
there after a turn (docs/LOCAL-MODELS.md). It never extracts a macOS Keychain entry.
`CONTROL_CLAUDE_AUTH_FILE` can point to another absolute, private credential
file, which is mounted read-only. Repeat `login` when the session expires.

An Anthropic API key instead (billed per use): `control accounts add claude NAME
--kind api-key`, then `pbpaste | control claude key --account NAME`. The key
stays on the host: Control's [key-injecting relay](PORTS.md#key-injecting-relay)
adds it to Claude's requests. Claude's container instead holds a per-attempt
placeholder at `/home/agent/.control-key/anthropic-api-key`, which
`--settings '{"apiKeyHelper":"cat <that file>"}'` has Claude read itself
(`--restricted` keeps `--settings`; its init event reports
`apiKeySource: apiKeyHelper`). The container has no network.
`ANTHROPIC_BASE_URL` points Claude at the relay on its loopback, and
`CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` keeps it from its other hosts.
Claude Code sends the placeholder in both `Authorization: Bearer` and
`x-api-key`; the relay strips both and sends the key as `x-api-key`. Verified
offline with the pinned image: its requests (`POST /v1/messages?beta=true`)
reached a fake provider with the canary key. Claude's built-in tools are
removed, and its shell runs in a separate container with no secret.

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
