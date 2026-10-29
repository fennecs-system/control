# Working on Control

Keep `main` ready for someone to clone, build and use. Make changes on a short-lived
feature branch, run the relevant local checks before pushing, and open a pull
request. The maintainer merges after its Rust checks pass; coding agents do not
merge pull requests. Deploy the merged revision when requested. Fix a failing
main build before starting another feature.

```sh
git switch main
git pull --ff-only
git switch -c feature/short-description
```

Use the tools pinned by `flake.lock`:

```sh
nix develop
cargo test --locked --workspace
cargo test --locked --manifest-path web/Cargo.toml
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked --manifest-path web/Cargo.toml --all-targets -- -D warnings
cd web
trunk build --release --locked
cd ..
cargo build --release --locked --bins
```

CI runs those checks on Linux and macOS for pull requests and pushes to `main`.
It needs no provider credentials and makes no model calls. The ignored browser,
container and authenticated model checks are explicit local checks; run the ones
relevant to changes in those boundaries. Report what was tested and any remaining
limits in the pull request. Keep credentials and personal node configuration
outside the checkout.

Require the `Rust (ubuntu-24.04)` and `Rust (macos-15)` checks on `main` in GitHub
branch rules, along with pull requests, no force pushes and no branch deletion.
Branch rules are repository settings; the workflow file does not enable them.
Keep review requirements practical for the current maintainers while preserving
the CI gate.
