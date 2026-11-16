# Local deployment on NixOS

Run Control as your normal user, with configuration and credentials in your home
directory. An `x86_64-linux` or `aarch64-linux` host builds its own worker images;
no separate Linux builder is needed.

## Prepare the machine

Add these settings to your NixOS configuration, replacing `yourUser` with your
login name:

```nix
{ pkgs, ... }: {
  nix.settings.experimental-features = [ "nix-command" "flakes" ];
  virtualisation.docker.enable = true;
  users.users.yourUser.extraGroups = [ "docker" ];
  environment.systemPackages = [ pkgs.git ];
}
```

Apply with `sudo nixos-rebuild switch`, then log out and back in so the Docker
group applies. Docker group membership is root-equivalent on this machine. Check
that `docker info` works as your normal user.

## Install and start

```sh
git clone https://github.com/fennecs-system/control.git
cd control
nix run .#setup
nix run .#codex -- -c 'cli_auth_credentials_store="file"' login
./target/release/control doctor
./target/release/control serve
```

Setup builds the package, loads and pins the worker image, initializes private
configuration and registers the checkout for `/improve`. It preserves existing
configuration, state and models, and never signs in or starts services. For
optional images and the `--full` worker profile, see [setup](setup.md#prerequisites).

In another terminal, run `./target/release/control open` for the web UI (or
`open --link` to print a single-use sign-in link), or `control tui` for terminal
chat. The server listens on `127.0.0.1:8080`, so no firewall change is needed.
Configuration lives in `~/.config/control/` and state in `~/.local/state/control/`.

The stable executable is `~/.local/share/control/package/bin/control` (under
`$XDG_DATA_HOME` if set). Keep the output links there; they protect the package
and images from garbage collection.

## Optional systemd user service

Stop the foreground server, then create `~/.config/systemd/user/control.service`:

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

The service uses your home directory, configuration, provider login and Docker
access. To keep it running after logout and start it at boot, run
`sudo loginctl enable-linger "$USER"`. Stop the service before running a
foreground server against the same state.

## Updating

Pull a reviewed revision, let active work finish, then rerun setup with the same
flags and restart:

```sh
nix run .#setup
systemctl --user restart control.service
"$HOME/.local/share/control/package/bin/control" doctor
"$HOME/.local/share/control/package/bin/control" tui --check
```

Do not remove Docker images still used by active runs or services.

## Build only

`nix build .#control` (or `nix build`) produces `./result/bin/control` without
loading images, changing pins or starting services. The build vendors both Cargo
lockfiles, builds the UI offline in its own derivation and runs the workspace
tests. Images build separately, for example `nix build .#worker-image`; setup
loads and pins them.

`nix build .#control-server` builds the same web/MCP server without the `tui`
feature or terminal dependencies.
