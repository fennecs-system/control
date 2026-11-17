# Declared build environments

The model worker and the project build environment are separate images. A
[workspace command](COMMAND-JOBS.md) selects the operator-installed build image
with `environment: "build"`. It runs offline with the task's writable workspace,
without provider credentials, the host Nix daemon or the Docker socket.

## Setup

From the Control checkout:

```sh
nix run .#setup -- --build-environment
```

Setup builds `tools/build/image.nix` with the pinned Nixpkgs, loads it and saves
its immutable image ID in `build-image-id` in the config directory
(`~/.config/control/` by default). `CONTROL_BUILD_IMAGE` overrides that pin; both
must be pinned by digest. Agents cannot choose image names, install host packages
or rebuild the image through a command tool.

The image contains rustc, Cargo, rustfmt, Clippy, lld, Trunk, the matching
wasm-bindgen CLI, Git, a C compiler, pkg-config, ripgrep and fd. Dependencies from
both Cargo lockfiles are fetched by checksum during the operator's Nix build and
vendored read-only. Cargo, and tools that launch Cargo such as Clippy, read the
image's offline source-replacement configuration (`CARGO_HOME=/opt/control-cargo`).
Jobs write only to their own workspace and scratch directories; nothing is
published to a shared image, package or compiler cache.

## Lockfile check

The image records each lockfile's SHA-256 in its `dev.control.build-locks` label.
`control doctor` compares those hashes with the source checkout without running a
model or container. Each build job repeats the check against its own workspace
before starting, so a stale image fails with a rebuild instruction instead of a
missing-crate error. Lockfiles absent from a task's workspace are skipped; present
ones must match.

Rerun setup after any lockfile change; updating the Control binary does not
update the separately pinned build image. Adding a crate that is not vendored
fails offline, so review the lockfile change and rebuild. Source-only changes need
no rebuild. A running job keeps its captured image. `/etc/control-environment.json`
inside the image records tool versions and lockfile hashes.

## Checking a Control change

Ask the agent to start a build-environment job with `timeout_seconds: 1800` and:

```sh
set -eu
cargo test --locked --workspace
cargo test --locked --manifest-path web/Cargo.toml
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --manifest-path web/Cargo.toml --all-targets -- -D warnings
(cd web && env -u NO_COLOR trunk build --release --locked)
cargo build --release --locked --bins
```

Follow progress with `control_workspace_status`. These commands use no provider
credentials; model calls and authenticated browser/container tests are separate
checks.

## Other Rust repositories

Import `tools/build/image.nix` from the repository's trusted Nix configuration
with its own `cargoLocks` list, and optionally `extraPackages` or `wasmBindgen`.
Each `cargoLocks` entry takes a `lockFile` and an optional `workspacePath` (such
as `web/Cargo.lock`) giving its path in the task workspace; otherwise the basename
is used. Paths must be unique. Only crates.io sources are supported; other source
types fail evaluation. Build and load the image, then set that node's
`build-image-id`. This declares a tool environment; it grants the agent no extra
authority over the repository.

The image is built with Nix but contains no Nix daemon and cannot fetch packages
at run time.
