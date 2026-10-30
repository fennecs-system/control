# Declared build environments

The model worker and the project build environment are separate tools. Build
commands select an operator-installed environment with `environment: "build"`.
They run in an offline container with the current task's writable workspace,
without provider credentials, the host Nix daemon or the Docker socket.

From your Control checkout, prepare the optional environment:

```sh
nix run .#setup -- --build-environment
```

Setup builds the `build-image` declaration using the pinned Nixpkgs revision,
loads it and stores its immutable image ID in `~/.config/control/build-image-id`.
`CONTROL_BUILD_IMAGE` can override that pin with an immutable image ID or digest.
Agents cannot select arbitrary image names, install host packages or rebuild this
declaration on the host through a command tool.

The default image supplies Rust, Clippy, rustfmt, the WASM standard library, Trunk,
the matching wasm-bindgen CLI, Git, a C compiler, ripgrep and fd. Dependencies from
both Cargo lockfiles are fetched by checksum during the operator's Nix build and
included as read-only sources. Cargo and its subprocesses read the image's Cargo
configuration automatically; build jobs have their own writable workspace outputs
and scratch directories. No job publishes
an image or writes to a shared package or compiler cache. Base images remain
immutable.

For a Control improvement, ask the agent to start a build-environment job with
`timeout_seconds: 1800` and this command:

```sh
set -eu
cargo test --locked --workspace
cargo test --locked --manifest-path web/Cargo.toml
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --manifest-path web/Cargo.toml --all-targets -- -D warnings
(cd web && env -u NO_COLOR trunk build --release --locked)
cargo build --release --locked --bins
```

Use the command job's status/output tools to follow long builds. Model calls and
authenticated browser/container tests are separate explicit checks; these commands
do not use provider credentials.

`/etc/control-environment.json` inside the build container records tool versions
and lockfile SHA-256 values. The job's image digest captures the complete installed
environment. Source changes need no image rebuild while they use the prepared
dependencies. Adding an uncached crate fails offline; review the lockfile change
and rerun setup to prepare a new image. A running job keeps its captured image.

For another Rust repository, import `tools/build/image.nix` in its trusted Nix
configuration with its own `cargoLocks` list and optional `extraPackages` or
`wasmBindgen`. Only crates.io lockfile sources are supported in this first version;
unsupported source types fail evaluation. Build/load the resulting image and set
that node's `build-image-id`. This is a declared tool environment, not a special
case that grants the agent authority over a particular repository.

Nix is used to construct the image. The image does not contain a Nix daemon or
offer arbitrary package fetching during execution. Future environment requests
can add capabilities through the same operator-controlled tool boundary.
