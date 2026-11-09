# Setup and configuration

Start with the [README quick start](../README.md). This guide covers the Linux
builder, manual builds, credential paths, configuration and optional Discord.

## Prerequisites

The current target is macOS with Nix flakes, Git, a running Linux
Docker-compatible engine and a Linux Nix builder. Match the image architecture:
`aarch64-linux` for Apple Silicon or `x86_64-linux` for Intel. A Linux host can
build its matching images locally; the invited setup currently targets macOS.

`nix run .#setup` builds the embedded Rust/WASM UI, the release binary and the
Codex worker image. It loads the image and saves its immutable ID. Add `--claude`
or `--browser` after `--` for optional images. Add `--build-environment` for the
[offline Rust/WASM build tool](BUILD-ENVIRONMENTS.md) used by self-improvement:

```sh
nix run .#setup -- --claude --browser
```

Existing configuration, state and model choices are preserved. Image pins change
only after every requested image builds and loads. Setup does not sign in, call
a model or start services. Repeat it after pulling source changes.

On a fresh node, `--claude` selects Claude `sonnet`; otherwise setup selects
Codex. Setup also registers this checkout for explicit `/improve` requests.

## Linux builder on macOS

Keep using an existing compatible Linux Nix builder if you have one. Otherwise:

```sh
nix run .#builder
```

This starts the pinned Nixpkgs VM bootstrap. Its upstream installer requests
`sudo` for builder SSH credentials. Follow the
[Nix builder instructions](https://nixos.org/manual/nixpkgs/stable/#sec-darwin-builder)
to configure the daemon's trusted user, builder and SSH connection, then restart
the daemon. Keep the VM running in its terminal and run setup in another.

The builder does not provide a Docker engine. Control's setup command does not
modify system configuration or silently provision a VM.

## Codex login

The pinned Codex CLI supports file-based credential storage:

```sh
nix run .#codex -- -c 'cli_auth_credentials_store="file"' login
```

You can also use your own installed Codex with file-based storage configured.
The source is `$CODEX_HOME/auth.json` or `~/.codex/auth.json` by default;
`CONTROL_CODEX_AUTH_FILE` can select another absolute path. It must be a private
regular file owned by a non-root user. Symlinks and group/world permissions are
rejected. Never print, paste or commit it.

Workers mount only that file read-only. It is not copied into images/workspaces
or injected through environment variables. The host checks metadata; the trusted
container adapter consumes the login. If authentication expires or needs a write
to refresh, repeat the supported login and submit a new run. The mount stays
read-only. Use your own account according to your provider's terms.

`control --check-worker` performs a small authenticated model/tool check and uses
model usage. `control doctor` instead runs offline prerequisites and isolation
probes without mounting real credentials or calling a model.

See [Claude setup](CLAUDE.md) to run Claude inside Control or connect your own
Claude Code to Control's MCP tools. The README also has the Codex MCP command.

## Private configuration

`control config init` creates private configuration from the placeholder
template without overwriting existing files. `control config path` prints the
resolved directory. In this guide, `control` means the built
`./target/release/control` or an installed copy.

For manual builds or older nodes missing the improvement source path, run
`control config init --source-root "$(pwd -P)"` from the Control checkout.
`--provider claude` can initialize a fresh model library with `sonnet`.
These options preserve existing source and model choices.

| Location or setting | Purpose |
| --- | --- |
| `~/.config/control/` | Default private configuration directory. |
| `~/.local/state/control/` | Default chats, notes, run ledger and outputs. |
| `CONTROL_CONFIG_DIR` | Absolute configuration-directory override. |
| `CONTROL_STATE_DIR` | Absolute state-directory override. |
| `worker-image-id` | Immutable Codex worker image ID. |
| `claude-image-id` / `browser-image-id` | Optional image IDs. |
| `build-image-id` / `CONTROL_BUILD_IMAGE` | Optional immutable offline build-tool image. |
| `model.json` | Saved models and default; managed by the web UI. |
| `source-root` / `CONTROL_SOURCE_ROOT` | Absolute source path for explicit improvements. |
| `worker-timeout-secs` / `CONTROL_WORKER_TIMEOUT_SECS` | Explicit attempt deadline, captured at admission and clamped to 60 s–24 h. When unset, ordinary attempts default to 600 s; approved persistent bots have no wall-clock deadline. Approval during an active attempt removes only its implicit default, never an explicit limit. |
| `CONTROL_ADDR` | Loopback bind address; default `127.0.0.1:8080`. |

Without explicit directory overrides, `XDG_CONFIG_HOME/control` and
`XDG_STATE_HOME/control` take precedence over the default home paths. Paths are
independent of the launch directory. Configuration directories use mode 0700;
private files use mode 0600. Repository-local `.env` and configuration are not loaded.

For an old checkout-local node, stop both services before migrating. Move
`.control/discord-access.json` to the configuration directory and `.env` to
`discord.env` there, without printing their contents. Move remaining state to
the state directory; never overwrite an existing destination. Preserve bindings.
The old `codex_owner_id` is accepted for migration but no longer grants access.

## Manual build and installation

From the checkout, as an alternative to the setup helper:

```sh
nix develop
(cd web && trunk build --release --locked)
env -u CARGO_TARGET_DIR cargo build --release --locked --bin control
./target/release/control config init
docker load -i "$(nix build .#packages.aarch64-linux.worker-image --no-link --print-out-paths)"
umask 077
docker image inspect control-worker:nix --format '{{.Id}}' > "$(./target/release/control config path)/worker-image-id"
```

Use `x86_64-linux` for Intel. The binary embeds the UI and security profile.
To put it in a stable location:

```sh
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/control "$HOME/.local/bin/control"
```

Use `~/.local/bin/control`, or add that directory to your shell's PATH. Reinstall
the binary after rebuilding. For development checks, see
[CONTRIBUTING.md](../CONTRIBUTING.md).

## Terminal chat

Run `control tui`; it starts a fresh chat without resuming or creating a saved
conversation until you send a message. It works from any directory. Use Enter to send, Alt+Enter for
a newline, Ctrl+N or `/new` for a new chat, Ctrl+Up/Down to switch chats, and
Ctrl+Q to quit. PageUp/PageDown scroll; End follows new replies.

`/chats` opens the conversation picker. `/lenses` opens a picker with Space to
toggle selections and Enter to apply. `/use engineering evidence` composes lenses;
`/use none` clears them. `/context` previews the next-message context, `/chat`
returns to the transcript, and Esc closes a picker. `/resume` opens the conversation
picker; `/resume ID` or `tui --resume ID` opens a conversation (`--session ID` is
also supported). `tui --check` verifies the connection without creating a chat.
`--server` accepts a loopback HTTP origin; its default follows `CONTROL_ADDR`.

## Background services

The [web](../hosting/dev.control.web.plist) and
[Discord](../hosting/dev.control.discord.plist) launchd files are templates.
Replace every `@…@` placeholder with your own paths and username before loading
them as user services. They contain no provider or Discord credential.

The website requires a local operator session. Run `control open` to open a
single-use sign-in link. Use `control open --link` to print the link and paste it
into a browser of your choice; the link works once. Discord access does not grant that session. Only
loopback binding is supported. Tailscale/remote publishing is not configured.

## Discord

Use a bot for this node. Configure `discord-access.json` with display labels,
numeric Discord user IDs, and `user` or `architect` roles. Labels alone grant
nothing. Unknown or duplicate identities are denied; keep `channel_id` null.
User-role identities can run tasks without source improvement or website
operator access. Multiple identities may belong to one person.

Store the token as `DISCORD_TOKEN="…"` in the private configuration directory's
`discord.env`; use mode 0600. Never put it in source or a service definition.
Only the explicitly activated bridge loads that file.

```sh
./target/release/control --bind-discord LABEL USER_ID
./target/release/control discord --check
./target/release/control discord --enable
```

Binding expects an existing configured label. `--check` verifies the bot and
provisions recipient channels without sending messages. `--enable` listens for
DMs and replies with links to the local chat. Server-channel messages are never
requested or processed. The bot and user may need a shared server and compatible
Discord privacy settings for DMs to work.

If delivery fails, run `control discord --diagnose`. To inspect server role and
channel permissions without reading messages, use
`control discord --audit-server-permissions`. A zero-permission invite can still
inherit @everyone permissions: deny View Channel for the bot where needed and
remove Administrator. A disabled Message Content intent alone is not an access
boundary.

Making a personal model subscription available to other people may violate
provider terms and risk suspension. Operators are responsible for compliant
configuration; Control roles do not establish contractual permission.
