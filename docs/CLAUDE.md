# Claude and Control

Both directions are supported. They use different credential and execution boundaries.

## Claude Code uses Control tools

Keep running your own Claude Code normally, with its own login and workspace.
Register the local Control binary as a stdio MCP server:

```sh
claude mcp add --transport stdio control -- /absolute/path/to/control mcp
```

Start Control first and reload Claude Code. The bridge uses the local operator
credential internally; do not put that credential in Claude configuration.
It exposes chat/lens/room/artifact reads, public search, the Rust playground and
browser capture. It does not expose job submission or Control administration.
Pass `--root /absolute/preview/directory` after `mcp` to select a browser input
directory. The operator's MCP bridge belongs on the trusted host, outside workers.

## Control runs Claude Code

This is the first container adapter implementation. Authenticated two-turn work,
restart continuation and artifact viewing were verified on macOS on 2026-10-29.
Codex remains the default.
It uses the unmodified Claude Code binary pinned by `flake.lock`, without a Node
service or a custom OAuth implementation. The Linux image is built directly with
Nix, using the same development tools as the Codex image.

On Apple Silicon with a Linux Nix builder and running container engine:

```sh
nix build .#packages.aarch64-linux.claude-image -o result-claude
docker load -i result-claude
control config init
umask 077
docker image inspect control-claude:nix --format '{{.Id}}' > "$HOME/.config/control/claude-image-id"
control claude check
control claude login
```

Use `x86_64-linux` for an Intel Mac. `check` exercises offline workspace isolation
without mounting a login or calling a model. `login` runs Claude's supported
interactive authentication inside the image. Follow its instructions in your
terminal/browser; never paste codes or credentials into chat. Private login files
live under `~/.config/control/claude/`. Control checks credential metadata and
mounts only `.credentials.json` read-only for task runs. It does not extract a
macOS Keychain entry. `CONTROL_CLAUDE_AUTH_FILE` can reference another absolute,
private credential-file path without copying it. Repeat login if the read-only session expires or needs
refresh; task runs cannot modify the stored login.

In **Manage → Models & providers**, click a saved Claude Code model to make it
the default. Each saved model stays visible, with the default marked. Use
**+ New model** to add another backend/model pair, or **Edit** on an existing row.
Older single-model settings preserve their selection and add the default model
for another installed backend (`sonnet` for Claude Code). The selection applies
to new runs. Existing runs and retries retain
their backend/model, and failures never fall back to another backend or the host.

Claude's built-in tools are disabled. It receives only Control's explicit MCP
tool list. `control_workspace_run` executes a bounded shell command in a separate,
offline container with the task workspace and no credentials, harness processes,
host configuration or Docker socket. The Claude harness sees a separate mailbox,
not the project tree. User/project settings, skills and other MCP servers are
not discovered. Node permissions still apply to every call. Workspace command
intent is recorded before execution; ambiguous acknowledgements are not replayed.
Search, Rust snippets and linked-room tools retain their existing broker policy.

The adapter translates bounded JSON stream events, final replies and token
usage into Control's run records. Cache reads are counted within total input;
missing telemetry stays unknown. Provider-reported costs are not presented as
subscription billing. Published artifacts and chat workspace checkpoints use
the same service as Codex.

Automatic cancellation, container reconciliation and hard live-workspace storage
quotas remain shared roadmap work. Auth-expiry/refresh and cancellation fault
testing remain open; the successful task and offline probe do not close the
full release gate.

Use your own model access according to the provider's terms. Multiple Discord
identities can belong to one person and retain user roles. Making a personal
subscription available to other people may violate those terms; node roles do
not establish contractual permission.

Reference: Anthropic's [programmatic CLI](https://code.claude.com/docs/en/headless),
[CLI flags](https://code.claude.com/docs/en/cli-reference),
[authentication](https://code.claude.com/docs/en/authentication), and
[hosting conditions](https://code.claude.com/docs/en/legal-and-compliance), checked
2026-10-29. Subscription runs do not use `--bare`, which skips subscription login.
