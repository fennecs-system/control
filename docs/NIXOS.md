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

## Build and initialize

```sh
git clone https://github.com/fennecs-system/control.git
cd control
mkdir -p "$HOME/.local/share/control"
nix build .#control --out-link "$HOME/.local/share/control/package"
control_bin="$HOME/.local/share/control/package/bin/control"
"$control_bin" config init --source-root "$(pwd -P)"
```

`nix build` without an attribute builds the same package. Nix vendors both Cargo
lockfiles, builds the UI offline with the matching wasm-bindgen CLI, embeds it in
the binary, and runs the native workspace tests. The UI is a separate derivation,
so backend-only edits reuse its output. Keep the named output links: they protect
the running package and images from Nix garbage collection.

Build and load the default Codex worker, then save its immutable image ID:

```sh
nix build .#worker-image --out-link "$HOME/.local/share/control/worker-image"
docker load -i "$HOME/.local/share/control/worker-image"
config_dir=$("$control_bin" config path)
umask 077
docker image inspect control-worker:nix --format '{{.Id}}' > "$config_dir/worker-image-id"
```

For the full worker (Nix, Node/npm, Java and Python inside containers), use
`worker-full-image` and tag `control-worker:nix-full` in those two commands.
Optional `claude-image`, `claude-full-image`, `browser-image` and `build-image`
outputs are also available; see [setup](setup.md) and [worker profiles](WORKER-PROFILES.md).
Rebuilding the host package alone does not update loaded images or their pins.

Sign in and check the node:

```sh
nix run .#codex -- -c 'cli_auth_credentials_store="file"' login
"$control_bin" doctor
"$control_bin" serve
```

In another terminal, run
`"$HOME/.local/share/control/package/bin/control" open` for the web UI or use
`tui` for terminal chat. `open --link` prints a one-use sign-in link if no desktop
browser is available. The server listens on `127.0.0.1:8080`; no firewall opening
is needed. Configuration defaults to `~/.config/control/` and state to
`~/.local/state/control/`, independent of the checkout or package path.

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

Pull a reviewed revision and repeat the package and image build/load commands.
Stop or finish active work before replacing image pins and restarting the service:

```sh
systemctl --user restart control.service
"$HOME/.local/share/control/package/bin/control" doctor
"$HOME/.local/share/control/package/bin/control" tui --check
```

Keep configuration, state and provider credentials outside the repository and
Nix store. Do not remove Docker images still used by active runs or services.
