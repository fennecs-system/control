# Browser tool

Control renders self-contained HTML previews in a Nix-built Firefox container.
The runner and MCP bridge are Rust. There is no distribution base or host browser
profile, and no Node/Python automation service.

Build through a compatible Linux Nix builder, then load and pin the resulting image:

```sh
nix build .#packages.aarch64-linux.browser-image --out-link /private/tmp/control-browser-image
docker load --input /private/tmp/control-browser-image
control config init
umask 077
docker image inspect --format '{{.Id}}' control-browser:nix > "$HOME/.config/control/browser-image-id"
```

Use `x86_64-linux` instead when appropriate. `flake.lock` pins the Nix inputs;
runs require an immutable image ID, never a mutable tag.

```sh
control browser capture path/to/preview.html 1280 800
```

In the local web UI, open **Tools → Browser** (`/tools/browser`), enter a
workspace-relative HTML path and viewport, then capture. The resulting PNG preview
and its download route require the same operator authentication as chat. Rendering
runs outside the shared chat-state lock.

The input must be an HTML file inside the current tool root. Only that file is
copied into a fresh private export and mounted read-only; inline CSS/images or
iframe `srcdoc` make previews self-contained. External assets and URLs cannot be
fetched. Inputs are UTF-8, regular, no-follow files capped at 4 MiB. Symlinks,
directories, credentials, host profiles, source trees and Docker sockets are not
browser inputs. The process receives no provider or operator credentials.

The container runs as UID/GID 65532 with a read-only root, dropped capabilities,
no privilege escalation, default Docker seccomp, no network, 1 GiB memory, two
CPUs, 512 tasks/threads, 128 MiB shared memory, and a 256 MiB temporary filesystem.
The inside supervisor creates an ephemeral profile and stops Firefox after 45
seconds even if the host client disconnects. The host stops the named container
if the operation exceeds 60 seconds. Unexpected process-control errors may still
require runtime cleanup; this is not a remote-job reconciliation implementation.

PNG output streams to the host, capped at 8 MiB; the browser never receives a
writable host output mount. Viewports are 240–2560 pixels per side. Captures are
private files in `~/.local/state/control/browser/`. An OS lock serializes captures, and admission
rejects new work at 64 retained files or when an 8 MiB capture would exceed a
256 MiB artifact budget. Remove old capture PNGs when needed; receipts/configuration
and user source are not automatically deleted.

Software rendering is the initial profile. GPU devices, privileged execution,
network access, interactive browser control and authenticated browsing are not
enabled. GPU access can later be an explicit, separately validated tool capability;
it is unnecessary for basic layout screenshots.

## Codex MCP

`control mcp --root /absolute/control/checkout` is a stdio MCP server for the local
operator. The optional `--root` bounds browser inputs; without it, the root is the
node's state directory. It exposes read-only chat/lens/room/artifact tools and `control.browser.capture`,
which accepts `input`, optional `width` and `height`, and returns PNG content plus
an artifact path. It also exposes `control.web.search` and `control.rust.run`.
It rejects tools that send messages, start agent runs or administer Control.
Capturing a preview writes only its bounded artifact; Rust snippets execute in
fresh offline Nix containers without host mounts or credentials.

The bridge consumes the existing operator credential internally to read the local
Hive. No token is placed in Codex configuration, MCP output or a browser container.
It must run on the trusted host, not inside an agent worker; existing worker room
MCP remains scoped to the worker's original chat actor and room. Its root directory
bounds browser file selection. The local web service must be running for Hive
reads; capture can operate independently with a configured image.

Register with Codex using its [documented stdio MCP setup](https://learn.chatgpt.com/docs/extend/mcp?surface=cli):

```sh
codex mcp add control-local -- /absolute/control/target/release/control mcp --root /absolute/control
```

This laptop additionally records a non-secret executable PATH for its Nix and
container runtime. Reload Codex to discover the server. This bridge does not add
its host tools to the already-running container workers or change their authority.

Verified on this laptop on 2026-10-29: pinned image
`sha256:e1acf7eff385c7d31a595cd1759041c0d5c05e7222de2c5b20d4034c2cd41c36`
rendered Control's own HTML preview through the CLI and returned a 390×844 PNG
through MCP. The MCP test checks image bytes, file equality and PNG dimensions.
Fontconfig's generic mono family resolves to DejaVu Sans Mono. The trusted-host
bridge also completed a real read-only room-directory request. No model calls,
Discord messages, GPU devices or operator credentials were used by Firefox.
