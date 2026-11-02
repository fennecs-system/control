# Worker profiles

`nix run .#setup` builds the default lite worker. `nix run .#setup -- --full`
selects the full worker; combine with `--claude` to build both full provider images.
Existing immutable image pins are replaced only after requested images load.
New attempts use the new pins; existing services keep their captured images and
policies. Start a new service to use the new profile.

Lite provides Rust development tools. Full additionally
bundles Nix, Node/npm, a headless JDK, Python and curl. These third-party runtimes
execute inside containers. Nix uses pinned local nixpkgs and a seeded single-user
database, without a daemon or host store/socket mounts.

Credential-free workspace commands using Full and new persistent services using
either profile have a private writable Docker layer. In Full, the Nix store directory and Nix state are
writable by their non-root user; preinstalled store paths remain root-owned,
protected by the sticky store directory. Model CLI containers stay read-only,
including when using a Full image. Use workspace tools for Nix package builds.
Store additions disappear with that container; they neither modify the image nor
become a cache shared with another service or attempt.

Local derivations and already available dependencies build offline. Full does not
grant internet access or make arbitrary remote nixpkgs packages reachable.
For example, `nix shell --offline --file '<nixpkgs>' nodejs --command node --version`
uses the bundled, registered Node package. Legacy `nix-shell -p nodejs` also
requests shell/stdenv dependencies that are not bundled; it cannot generally
complete offline even when Node itself is available.
Network access remains separately scoped. Nix's internal build sandbox is disabled
because the enclosing container supplies the isolation boundary; builds cannot
obtain privileges, host credentials or host daemon access.

Full persistent services use 2 GiB RAM and 128 processes; Lite uses 512 MiB RAM and
64 processes. New services have a disk-backed executable workspace and home, with
no default disk cap. `/tmp` remains 128 MiB of RAM-backed disposable scratch.
Both retain one CPU and their service lease.
An optional `storage_limit_mb` requires Docker quota support and limits only the
private writable layer, excluding mounts. Existing services keep their captured
storage policy. See [persistent tools](PERSISTENT-TOOLS.md).
Workspace commands retain their existing 2 GiB/256-process limits. Nix's private
store consumes the container engine's disk and is included in any service
writable-layer quota. There is no cross-container cache in this preset.
Large builds may need a different declared
build environment rather than these defaults.
