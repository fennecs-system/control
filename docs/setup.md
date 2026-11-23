# Setup and configuration

Start with the [README quick start](../README.md). This guide covers the Linux
builder, manual builds, credentials, configuration, background services and
Discord. On Linux/NixOS, use the [NixOS guide](NIXOS.md).

## Prerequisites

On macOS you need Nix with flakes, Git, a running Docker-compatible Linux engine
and a Linux Nix builder of the matching architecture: `aarch64-linux` for Apple
Silicon, `x86_64-linux` for Intel. A Linux host builds its own images.

`nix run .#setup` builds the Nix package (with the embedded Rust/WASM UI) and the
Codex worker image, loads the image and saves its immutable ID. Optional flags
after `--`:

| Flag | Adds |
| --- | --- |
| `--claude` | Claude Code image; selects Claude `sonnet` on a fresh node ([Claude](CLAUDE.md)). |
| `--grok` | Grok Build image, built from xAI's published source ([Grok](GROK.md)). The node default is unchanged. |
| `--antigravity` | Antigravity CLI image, Google's release binary pinned by hash ([Antigravity](ANTIGRAVITY.md)). The node default is unchanged. |
| `--full` | Nix, Node/npm, JDK, Python and curl in worker images ([worker profiles](WORKER-PROFILES.md)). |
| `--browser` | Offline Firefox capture image ([web access](WEB-ACCESS.md)). |
| `--build-environment` | [Offline Rust/WASM build image](BUILD-ENVIRONMENTS.md) for self-improvement. |
| `--solver` | [HiGHS, Z3 and CP-SAT solvers](SOLVERS.md). |

```sh
nix run .#setup -- --claude --browser
```

Setup preserves existing configuration, state and model choices. Image pins
change only after every requested image builds and loads. It does not sign in,
call a model or start services. Rerun it after pulling changes.

Output links under `${XDG_DATA_HOME:-$HOME/.local/share}/control/` protect the
package and images from garbage collection; `package/bin/control` there is the
stable executable. Setup also copies it to `target/release/control` and registers
the checkout as the source root for `/improve`.

## Linux builder on macOS

Keep an existing compatible Linux Nix builder if you have one. Otherwise run:

```sh
nix run .#builder
```

This starts the pinned Nixpkgs builder VM. Its installer asks for `sudo` to create
builder SSH credentials. Follow the
[Nix builder instructions](https://nixos.org/manual/nixpkgs/stable/#sec-darwin-builder)
to configure the daemon's trusted user, builder and SSH connection, then restart
the daemon. Keep the VM running in one terminal and run setup in another.

The builder does not provide a Docker engine. Setup never changes system
configuration or provisions a VM.

## Codex login

```sh
nix run .#codex -- -c 'cli_auth_credentials_store="file"' login
```

Your own Codex install also works if it uses file-based credential storage.
Control reads `$CODEX_HOME/auth.json`, else `~/.codex/auth.json`;
`CONTROL_CODEX_AUTH_FILE` selects another absolute path. The file must be a
private regular file owned by a non-root user; symlinks and group/world
permissions are rejected. Never print, paste or commit it.

Workers mount only that file, read-only. It is never copied into images or
workspaces or passed through environment variables. If the login expires, repeat
the login and submit a new run.

`control doctor` runs offline prerequisite and isolation checks without real
credentials or model calls. `control --check-worker` runs one authenticated
model/tool check and uses model quota.

## Private configuration

`control` below means `./target/release/control` or an installed copy.
`control config init` creates private configuration from a template without
overwriting existing files; `control config path` prints the directory.
`--source-root PATH` sets the improvement source and `--provider codex|claude|grok|antigravity`
initializes a fresh model library; both preserve existing choices.

| Location or setting | Purpose |
| --- | --- |
| `~/.config/control/` | Default configuration directory. |
| `~/.local/state/control/` | Default chats, notes, run ledger and outputs. |
| `state.json` + `state.log` | Node state: a snapshot plus one JSON line per save ([inspect with jq](HISTORY.md#inspecting-the-state)). Copy both, or stop Control first (a clean stop folds the log into `state.json`). |
| `CONTROL_CONFIG_DIR` / `CONTROL_STATE_DIR` | Absolute directory overrides. |
| `worker-image-id` | Codex worker image ID. |
| `claude-image-id`, `grok-image-id`, `antigravity-image-id`, `browser-image-id`, `solver-image-id` | Optional image IDs. |
| `build-image-id` / `CONTROL_BUILD_IMAGE` | Optional offline build-tool image. |
| `model.json` | Saved models and default; managed in the web UI. |
| `source-root` / `CONTROL_SOURCE_ROOT` | Absolute source path for `/improve`. |
| `worker-timeout-secs` / `CONTROL_WORKER_TIMEOUT_SECS` | Explicit attempt limit for every attempt, 60 s–24 h. When unset, see the two settings below; other attempts stop at 600 s. |
| `bot-turn-timeout-secs` / `CONTROL_BOT_TURN_TIMEOUT_SECS` | Turn limit for approved persistent bots; default 1200, 60 s–24 h, `0` or `none` for unlimited. |
| `local-turn-timeout-secs` / `CONTROL_LOCAL_TURN_TIMEOUT_SECS` | Turn limit for any attempt on a local model; default 3600, 60 s–24 h, `0` or `none` for unlimited. See [local agent turns](LOCAL-MODELS.md#local-agent-turns). |
| `service-memory-gib` | Service pool memory (sum of service limits); default 20, 1–4096. |
| `service-cpus` | Service pool CPUs; default 8, 1–1024. |
| `service-cpu-oversubscription` | Multiplier on `service-cpus` for admission; default 2, 1–8. Containers keep hard CPU caps. |
| `service-pids-per-gib` | Process limit per GiB of a service's memory, never below the image default (Lite 64, Full 128); default 64, 1–4096. |
| `service-pids-max` | Ceiling on a service's process limit; default 1024, 128–65536. |
| `service-commands-max` | Concurrent `control_service_exec` commands per service, also bounded by its process limit; default 64, 1–1024. |
| `CONTROL_ADDR` | Loopback bind address; default `127.0.0.1:8080`. |

Timeout limits are captured when an attempt is admitted. An explicit
`worker-timeout-secs` wins; otherwise local-model attempts use the local limit and
approved persistent bots use the bot limit. Bots continue across turns through
handoffs.

Without directory overrides, `XDG_CONFIG_HOME/control` and `XDG_STATE_HOME/control`
take precedence over the home defaults. Paths do not depend on the launch
directory. Directories use mode 0700 and private files 0600. Repository-local
`.env` and configuration files are never loaded.

## Manual build and installation

An alternative to `nix run .#setup`, from the checkout:

```sh
nix develop
(cd web && trunk build --release --locked)
env -u CARGO_TARGET_DIR cargo build --release --locked --bin control
./target/release/control config init
docker load -i "$(nix build .#packages.aarch64-linux.worker-image --no-link --print-out-paths)"
umask 077
docker image inspect control-worker:nix --format '{{.Id}}' > "$(./target/release/control config path)/worker-image-id"
```

Use `x86_64-linux` on Intel. The binary embeds the UI and security profile. To
install it, `install -m 755 target/release/control "$HOME/.local/bin/control"`
and reinstall after each rebuild. Development checks are in
[CONTRIBUTING.md](../CONTRIBUTING.md).

`control --version --json` identifies the installed binary; `GET /health/build`
identifies the running one with the same fields (`version`, `revision`, `dirty`,
`arch`, `os`). Nix builds record the source revision and whether it was modified.
Plain Cargo builds report `revision: null` unless `CONTROL_BUILD_REVISION=<full-sha>`
(or `<full-sha>-dirty`) is set. An unknown or dirty build does not verify an exact
merged revision. These fields identify a build; they are not signed provenance.

## Terminal chat

`control tui` opens a fresh chat from any directory; nothing is saved until you
send a message. Enter sends, Alt+Enter adds a newline, Ctrl+N or `/new` starts a
chat, Ctrl+Up/Down switches chats, PageUp/PageDown scroll, End follows replies and
Ctrl+Q quits. Type `/` for command suggestions and `/help` for the full list.

`/resume ID` or `tui --resume ID` (also `--session ID`) opens a conversation.
`tui --check` verifies the connection without creating a chat. `--server` takes a
loopback HTTP origin and defaults to `CONTROL_ADDR`. See
[TERMINAL.md](TERMINAL.md) for development workflows.

## Background services

The [web](../hosting/dev.control.web.plist) and
[Discord](../hosting/dev.control.discord.plist) launchd files are templates.
Replace every `@…@` placeholder with your own paths and username before loading
them as user services. They contain no credentials. For systemd, see
[NixOS](NIXOS.md#optional-systemd-user-service).

The website needs a local operator session. `control open` opens a single-use
sign-in link; `control open --link` prints it instead. Discord access does not
grant that session. Only loopback binding is supported.

## Discord

Use a dedicated bot for this node. List identities in `discord-access.json` with
display labels, numeric Discord user IDs and a `user` or `architect` role. Labels
alone grant nothing; unknown or duplicate identities are denied. Keep
`channel_id` null. The `user` role can run tasks but cannot request source
improvements or use the website. One person may have several identities.

Store the token as `DISCORD_TOKEN="…"` in `discord.env` in the configuration
directory, mode 0600. Never put it in source or a service file. Only the
activated bridge loads it.

```sh
control --bind-discord LABEL USER_ID
control discord --check
control discord --enable
```

Binding needs an existing label. `--check` verifies the bot and prepares DM
channels without sending messages. `--enable` listens for DMs and replies with
links to the local chat. Server-channel messages are never requested or read. DMs
may need a shared server and compatible privacy settings.

### Operator requests and steering

Discord is the first adapter over the generic channel core (`client::channel`),
which reaches Control only through the same operator interface as `control tui`.

Forwarding operator requests is **off by default**. Enable it and set the command
prefix on the web Connections page (`/?view=integrations`). It saves
`channels.json` in the configuration directory, which adapters re-read on every
poll:

```json
{"discord":{"enabled":true,"kinds":["question","bot","port"],"chats":[],"prefix":"!"}}
```

`kinds` limits what is forwarded; `chats` lists top-level chat IDs (empty means
all). The prefix defaults to `!` because Discord reserves `/`. Send `!help` for
the command list.

`kinds` also accepts `notice`: short operator notices from opted-in agents (an
[antibody](LENSES.md#antibodies) or a lead), forwarded once; reply to steer the
team that sent it. Add `"brief":true` (and optionally `"brief_hour":8`, UTC) to
receive the node's daily intel brief once a day; it is also saved under the state
directory as `intel/brief-DATE.md`. Edit `channels.json` directly for these.

A channel adapter implements `Transport` in `crates/client/src/channel.rs`: `post`
(text with an idempotency key), and optionally `edit`, `post_request` and
`settle`, with `capabilities` such as `max_text`. Briefs and notices use only
`post`, so a report sink such as a Google Workspace adapter (append to a Doc,
send a Gmail digest) needs no core change: implement `Transport`, add it to
`ADAPTERS` and configure it by name in `channels.json`.

When forwarding is on, architects receive design questions, bot requests and
port approvals from every chat. Reply to the request message (an option number
or text for a question, `allow`/`deny` for a bot, `allow`/`allow --loopback`/`deny`
for a port), send the command it shows, or press its buttons. Every path runs the
same decision through the local operator session, so the server rechecks that
the request is still pending and rejects duplicates. A decided message turns
green or grey and loses its buttons; an expired or already decided request shows
"No longer pending".

Buttons use a gateway connection opened by `control discord --enable`, with no
privileged intents. Leave the application's **Interactions Endpoint URL** empty
in the Discord developer portal, or presses go there instead. If the gateway is
down, typed replies still work.

Architects can steer any chat whether or not forwarding is on: `!chats` lists
recent DMs, `!resume ID` selects one, `!new` starts one, `!genes` lists genes and
`!use GENE…` sets the DM's DNA (`!use none` clears). Messages then go to the selected DM, and `!steer`,
`!cancel`, `!btw` and the other chat commands work as in the TUI. Replies are
relayed back. `!resume` with the DM's own chat ID returns to it. Routing state is
kept in `channels/NAME.json` in the state directory.

If delivery fails, run `control discord --diagnose`. To inspect server roles and
channel permissions without reading messages, run
`control discord --audit-server-permissions`. A zero-permission invite can still
inherit @everyone permissions: deny View Channel where needed and remove
Administrator. Disabling the Message Content intent is not an access boundary.

Sharing a personal model subscription with other people may violate provider
terms. Control roles do not establish contractual permission.
