# Local deployment on NixOS

Control's flake builds the native binary with its embedded Rust/WASM UI and
separate Linux worker images. Run Control as your normal user, with configuration
and credentials in your home directory. No separate Linux builder is needed on
an `x86_64-linux` or `aarch64-linux` host of the matching architecture.

## Prepare the machine

Add these settings to your NixOS configuration, replacing `yourUser` with your
existing login name:

```nix
{ pkgs, ... }: {
  nix.settings.experimental-features = [ "nix-command" "flakes" ];
  virtualisation.docker.enable = true;
  users.users.yourUser.extraGroups = [ "docker" ];
  environment.systemPackages = [ pkgs.git ];
}
```

Apply with `sudo nixos-rebuild switch`, then log out and back in for Docker group
membership to take effect. Docker group membership grants root-equivalent access
to this machine. Verify `docker info` succeeds as your normal user before continuing.

## Install and start

```sh
git clone https://github.com/fennecs-system/control.git
cd control
nix run .#setup
nix run .#codex -- -c 'cli_auth_credentials_store="file"' login
./target/release/control doctor
./target/release/control serve
```

Setup builds the native Nix package and its embedded UI, loads the worker image,
saves its immutable ID and initializes private configuration. It also registers
the checkout for explicit improvement requests. Existing configuration, state and
model selection are preserved. Login and service startup are explicit.

For Nix, Node/npm, Java and Python inside worker containers, use
`nix run .#setup -- --full`. Optional `--claude`, `--browser` and
`--build-environment` flags add those images; see [setup](setup.md).

In another terminal, run `./target/release/control open` for the web UI or use
`tui` for terminal chat. `open --link` prints a one-use sign-in link if no desktop
browser is available. The server listens on `127.0.0.1:8080`; no firewall opening
is needed. Configuration defaults to `~/.config/control/` and state to
`~/.local/state/control/`, independent of the checkout or package path.

Setup retains Nix output links under `${XDG_DATA_HOME:-$HOME/.local/share}/control/`.
The stable executable is `package/bin/control` there; a copy remains at
`target/release/control` for the checkout commands above. Keep the output links
to protect the package's runtime libraries and images from garbage collection.

## Optional systemd user service

Stop the foreground server first, then create
`~/.config/systemd/user/control.service`:

```ini
[Unit]
Description=Control swarm manager

[Service]
ExecStart=%h/.local/share/control/package/bin/control serve
Environment=PATH=/run/current-system/sw/bin
Environment=CONTROL_ADDR=127.0.0.1:8080
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now control.service
systemctl --user status control.service
journalctl --user -u control.service -f
```

The service uses your existing home directory, configuration, provider login
and Docker access. For operation after logout and startup at boot, enable
lingering with `sudo loginctl enable-linger "$USER"`. Stop it with
`systemctl --user stop control.service` before running a foreground server against
the same state.

## Updating

Pull a reviewed revision and rerun setup with the same image flags. Stop or finish
active work before replacing image pins and restarting the service:

```sh
nix run .#setup
systemctl --user restart control.service
"$HOME/.local/share/control/package/bin/control" doctor
"$HOME/.local/share/control/package/bin/control" tui --check
```

Keep configuration, state and provider credentials outside the repository and
Nix store. Do not remove Docker images still used by active runs or services.

## Build only

For inspection or custom deployment, `nix build .#control` (or just `nix build`)
produces `./result/bin/control`. This does not load images, change pins or start
services. Nix vendors both Cargo lockfiles, builds the UI offline with matching
wasm-bindgen, and runs the native workspace tests. The UI has a separate derivation
so backend-only edits reuse its output. Existing image outputs remain available,
for example `nix build .#worker-image`; normal installation uses setup to load and
pin them automatically.

For a daemon without terminal dependencies, `nix build .#control-server` produces
the same web/MCP server with the `tui` feature disabled. It retains the embedded
web UI and reports that `control tui` is unavailable in this variant. The default
package and setup command retain terminal support. Neither build starts services.
