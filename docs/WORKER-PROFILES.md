# Worker profiles

`nix run .#setup` builds the default **Lite** worker. `nix run .#setup -- --full`
builds the **Full** worker; add `--claude` to build the Full Claude image too.
Image pins change only after every requested image has built and loaded. New
attempts and services use the new pins; running services keep the image and
policy they started with, so start a new service to pick up a new profile or newly
added tools.

| | Lite | Full |
| --- | --- | --- |
| Tools | rustc, Cargo, rustfmt, Clippy, Git, a C compiler, ripgrep, fd, Bash, coreutils, procps (`ps`) and common archive and text tools | Lite plus Nix, Node/npm, a headless JDK, Python and curl |
| Image label | none or `lite` | `dev.control.worker-profile=full-v1` |
| Persistent service default | 512 MiB RAM, 1 CPU, 64 processes | 2 GiB RAM, 1 CPU, 128 processes |

The package lists are in `examples/worker-image.nix`. Third-party runtimes run
only inside containers.

## Writable layer

Credential-free workspace commands on a Full image, and new
[persistent services](PERSISTENT-TOOLS.md) on either profile, get a private
writable container layer. Model CLI containers stay read-only even on a Full
image, so use workspace tools for Nix builds. In Full, the Nix store and Nix state
are writable by the non-root user; preinstalled store paths stay root-owned,
protected by the sticky store directory. Anything added to the store disappears
with its container: it does not change the image or become a cache shared with
another service or attempt.

## Nix in Full

Full ships pinned local nixpkgs and a seeded single-user Nix database, with no
daemon and no host store or socket. Local derivations and bundled dependencies
build offline. Full grants no internet access, so arbitrary remote nixpkgs
packages are unavailable. For example,
`nix shell --offline --file '<nixpkgs>' nodejs --command node --version` works
because Node is bundled. `nix-shell -p nodejs` also needs shell/stdenv inputs that
are not bundled and generally cannot complete offline. Network access is granted
separately.

Nix's own build sandbox is disabled (`sandbox = false`) because the container is
the isolation boundary; builds cannot gain privileges, host credentials or host
daemon access.

## Resource limits

The service defaults above are floors. With more RAM (for example a service
`shape`), the process limit scales with memory; see
[persistent tools](PERSISTENT-TOOLS.md). New services keep task data in a
disk-backed `/workspace` volume with no default disk cap; `/tmp` is 128 MiB of
RAM-backed scratch. An optional `storage_limit_mb` caps only the private writable
layer (not mounts) and needs Docker storage-driver quota support. Nix store
additions count toward that layer. Existing services keep the storage policy they
started with.

[Workspace command](COMMAND-JOBS.md) limits do not depend on the profile. Large
builds should use the [declared build environment](BUILD-ENVIRONMENTS.md).
