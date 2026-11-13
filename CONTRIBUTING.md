# Working on Control

Keep `main` ready for someone to clone, build and use. Make changes on a short-lived
feature branch, run the relevant local checks before pushing, and open a pull
request. The maintainer merges after its Rust checks pass; coding agents do not
merge pull requests. Deploy the merged revision when requested. Fix a failing
main build before starting another feature.

After a successful deployment and health checks, follow the cleanup rule in
[AGENTS.md](AGENTS.md): remove obsolete clean worktrees, verified merged branches
and their old scratch files. Preserve unrelated or unmerged work and current
diagnostics. Do not perform this cleanup before deployment succeeds.

```sh
git switch main
git pull --ff-only
git switch -c feature/short-description
```

Use the tools pinned by `flake.lock`:

The Nix environment also supplies the wasm-bindgen CLI matching `web/Cargo.lock`,
so Trunk uses the installed tool instead of downloading a GitHub release archive.
Keep that CLI version aligned when updating the web lockfile.

```sh
nix develop
cargo test --locked --workspace
cargo test --locked --manifest-path web/Cargo.toml
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --no-default-features --lib --bin control
cargo clippy --locked --no-default-features --lib --bin control -- -D warnings
cargo clippy --locked --manifest-path web/Cargo.toml --all-targets -- -D warnings
cd web
trunk build --release --locked
cd ..
cargo build --release --locked --bins
```

Keep substantial unit-test suites beside their implementation: `file.rs` uses
`#[cfg(test)] mod tests;` to load `file/tests.rs`. They retain private access and
the same module paths. Keep short usage/contract documentation near the API and
point to the adjacent tests for complete examples. Use top-level `tests/` for
integration tests that need public interfaces or a separate process. Splitting
source files helps navigation/context; it does not itself create a compilation
boundary or remove test code from production (the cfg already does that).
Apply this incrementally when working on a module, rather than moving every
suite as unrelated churn.

CI runs those checks on Linux and macOS for pull requests and pushes to `main`.
The default `tui` feature preserves `control tui` and `control-harness`. The
server-only build omits terminal presentation and its dependencies; CI also
checks that its normal dependency graph contains neither ratatui nor crossterm.
The shared MCP client remains available to the server without terminal code.
It needs no provider credentials and makes no model calls. The ignored browser,
container and authenticated model checks are explicit local checks; run the ones
relevant to changes in those boundaries. Report what was tested and any remaining
limits in the pull request. Keep credentials and personal node configuration
outside the checkout.

CI restores separate caches for the Nix store and evaluation data, Cargo registry
archives and Git downloads, Trunk tools, both workspaces' compiled targets, and
the finished WASM bundle. Keys separate runner OS/architecture and toolchains.
Target caches save each revision and restore compatible dependencies from earlier
revisions, including when a lockfile changes. Cargo still checks freshness and all
tests and Clippy checks always run. CI disables debug symbols and incremental
compilation to reduce build time and cache size.

Linux Trunk tools use a dedicated XDG cache under the runner's temporary directory,
which is included in the downloads cache. This avoids depending on permissions of
a restored home-cache parent. macOS retains its native Library/Caches location.

Only an exact bundle cache hit skips Trunk. Its key covers the UI sources,
configuration, lockfile, shared context crate, Nix environment and workflow; a
backend-only change can reuse the UI. There is no fallback for finished bundles.
Nix caches have one immutable key per environment rather than per source commit;
they retain flake inputs as well as tool outputs without garbage collecting the
inputs before upload. The first run fills caches; later runs still rebuild any
changed Rust code. GitHub can evict caches when the repository quota is reached.

The current workflow does not build Docker images, so Docker/BuildKit layer
caching would add no benefit here. If image jobs are added, cache their Nix store
derivations (the images are declared in Nix). Compiler caches such as sccache are
another option if timings show target-cache transfers cost more than they save;
avoid duplicating both caches without measuring. A shared Cachix/FlakeHub binary
cache can also share Nix outputs outside GitHub, but needs separate configuration.

Dependabot checks action pins and both Cargo workspaces weekly, grouping version
updates by ecosystem to reduce redundant CI runs. Security updates have separate
groups when Dependabot security updates are enabled in repository settings.
Action versions stay pinned to release commits; Dependabot can update those pins.

Require the `Rust (ubuntu-24.04)` and `Rust (macos-15)` checks on `main` in GitHub
branch rules, along with pull requests, no force pushes and no branch deletion.
Branch rules are repository settings; the workflow file does not enable them.
Keep review requirements practical for the current maintainers while preserving
the CI gate.

The separate [codebase metrics](docs/CODE-METRICS.md) workflow reports source size,
complexity hotspots and clone candidates against the base revision. It is advisory;
do not make it a size/complexity merge gate. Changes to its standalone Rust tool
should run its locked tests and strict Clippy with
`--manifest-path tools/code-metrics/Cargo.toml`, plus a real snapshot comparison.
