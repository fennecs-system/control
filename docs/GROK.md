# Grok and Control

Control can run xAI's [Grok Build](https://github.com/xai-org/grok-build) CLI as
a worker, selected per chat or spawn as `grok:MODEL` (for example
`/model grok:grok-4.7`). Codex remains the default backend.

The image builds the unmodified CLI from xAI's published source (Apache-2.0) at
the revision pinned in `tools/grok/package.nix`, with the same development tools
as the other worker images. No installer script or prebuilt binary is used.

With a running container engine and a Linux Nix builder that has at least 8 GiB
of memory (the CLI is about 1,300 crates, and the stock macOS builder VM's 3 GiB
is not enough for its largest one):

```sh
nix run .#setup -- --grok
./result/bin/control grok check
./result/bin/control grok login
```

`check` tests offline workspace isolation without mounting a login or calling a
model, then mounts a fake credential in a turn container exactly as a turn
mounts the login and prints one PASS or FAIL line per reader: the CLI uid can
read it; another uid cannot, directly or through `/proc/1/root`, and it is not
in the environment. Run it after any image update. `login` runs Grok's own device-code sign-in (`grok login --device-auth`)
inside the image: open the address it prints and enter its code; never paste
codes or credentials into chat. The login lives under `~/.config/control/grok/`.
Control checks its metadata and mounts that folder, writable, at
`/home/agent/.grok-login` (`GROK_AUTH_PATH`), so a token Grok refreshes is kept;
only `auth.json`, its lock and Grok's in-flight copies stay there after a turn
(docs/LOCAL-MODELS.md). `CONTROL_GROK_AUTH_FILE` can point to another absolute,
private credential file, which is mounted read-only. Repeat `login` when the
session expires.

An xAI API key instead (billed per use): `control accounts add grok NAME --kind
api-key`, then `pbpaste | control grok key --account NAME`. Grok documents the
`XAI_API_KEY` variable as its API-key path (docs/user-guide/02-authentication.md
at the pinned revision), so the key is mounted read-only at
`/home/agent/.control-key/xai-api-key` and the image's `control-grok` entrypoint
exports it for Grok's process alone: it is never a variable of the container
or in the launch description. Control's MCP server is started through
`env -u XAI_API_KEY`, because Grok passes its environment to MCP servers. Not
yet confirmed with a real key.

## How a turn runs

Grok is run like Claude Code and given what Codex is given:

- The same captured turn context and policy prompt as Codex, delivered as a
  private file (`--prompt-file`); headless Grok does not read standard input.
- Its own tools are removed: `--tools mcp__control` allows no built-in tool,
  with subagents, web search, memory and plan mode off. What remains is Grok's
  two MCP meta-tools, `search_tool` and `use_tool`, and the one MCP server in its
  configuration: Control's. A turn is stopped if the stream lists or calls any
  other tool.
- Shell commands and builds run in separate offline containers through
  Control's workspace tools; the workspace is not mounted beside the login.
- Its home is a tmpfs, so sessions and logs die with the container. Control's
  history and handoffs are the only memory between turns.
- Events come from `--output-format streaming-json`; token usage is reported per
  response and in total at the end.

A failure never falls back to another backend or the host.

## Not yet confirmed against a live account

Written from the CLI's source and checked by tests on recorded shapes, not run
with a login: which model names an account may select, how a usage limit is
worded in the stream, and how often a refresh during a turn rewrites
`auth.json` (its folder is mounted writable, so a refresh is kept).
