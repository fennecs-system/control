# Rust analysis tool

A first tool container for Control: Rust formatting and optional Clippy, built from
Nix packages without a distribution base. The launcher is standalone Rust with no
third-party dependencies. This is a local command today; MCP registration and run
ledger integration remain future work.

Build the launcher on the host:

```sh
rustc --edition=2021 -C opt-level=2 tools/rust-analysis/main.rs -o /tmp/control-analyze
```

From the checkout root, build through a Linux Nix builder, then load the image:

```sh
nix build --impure --expr "let pkgs = import (builtins.getFlake \"nixpkgs\").outPath { system = \"aarch64-linux\"; }; in import $PWD/tools/rust-analysis/image.nix { inherit pkgs; }" --out-link /tmp/control-analysis-image
docker load --input /tmp/control-analysis-image
docker image inspect --format '{{.Id}}' control-analysis:nix
```

Pass that immutable `sha256:…` image ID to the launcher:

```sh
/tmp/control-analyze sha256:IMAGE_ID /path/to/rust/project /tmp/control-analysis-results fmt
```

The source must have a `Cargo.toml`. The output directory must sit outside the
source tree. Results are `result.json` (check, success, exit code, elapsed time)
and `report.txt` (Cargo diagnostics). Exit status is 0 for success, 1 for a failed
check, and 2 for an orchestration error. The launcher removes its temporary source
snapshot after Docker exits and stops the container after five minutes.

The launcher exports only Rust files, `Cargo.toml`, and `Cargo.lock`. It skips
hidden directories/files, symlinks, targets, dependency directories, and compiled
web assets. It never mounts the original source tree, `.env`, host Codex
credentials, Docker socket, or host Cargo configuration into the tool. Check the
source you submit: secrets embedded directly in Rust or manifests are still source
content, and source exports should not be changed concurrently during collection.

The snapshot is mounted read-only. A separate output directory is writable. The
tool runs as the invoking non-root user with a read-only container filesystem,
all Linux capabilities dropped, no privilege escalation, no network, resource
limits, and Docker's default seccomp policy. No extra kernel privileges are needed.

`fmt` works without downloading dependencies. `clippy` additionally uses
`--locked --offline`, and runs project build scripts inside this container. Its
empty Cargo cache means external dependencies must first be supplied through a
separate, pinned Nix dependency derivation; it will fail clearly when missing.
The narrow export also omits non-Rust build resources. This is intentionally a
small first tool, rather than a claim that arbitrary projects can already build.

The `nixpkgs` lookup follows the laptop's existing registry; the resulting image ID
pins execution, but repeatable rebuilding still needs a repository flake lock and
dependency inputs. Do not substitute a mutable image tag in a run.

Verified locally on 2026-10-28: image
`sha256:d1b9a32b92778f7c00838440e0d3f5c51e8a724e282561ac8793578d8e3a7b41`
passed formatting and Clippy on a dependency-free fixture. A deliberate Clippy
failure returned a nonzero exit and `success:false`. Two launcher safety tests passed.

## Profiling next

Control now exposes an optimized std-only microbenchmark through
`control.rust.bench`, using the existing offline playground container. See
[performance checks](../../docs/PERFORMANCE.md) for MCP inputs, limits and
whole-project benchmark commands. This does not add MCP execution to the older
standalone formatting launcher above.

Flamegraphs need an actual workload and sampled stacks. Linux `perf` additionally
depends on the VM kernel's performance events, host policy, and symbol support;
macOS containers use the Linux VM, so they cannot sample native macOS processes.
We have not verified those facilities. Do not grant `--privileged`, `SYS_ADMIN`,
or host process access to make a profile work.

A future profile tool should take a declared benchmark, pinned build inputs, a
bounded duration, and an explicit profiling permission; produce SVG plus raw
samples in the output mount; and record sampling overhead and unavailable kernel
features. Keep that capability separate from this no-network analysis tool.
