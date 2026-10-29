# Repository workflow

Use a feature branch for changes. Open a pull request and merge into `main` after
the Rust CI checks pass. Keep `main` buildable; fix a failed main check before
starting another feature. Deploy the merged revision. See [CONTRIBUTING.md](CONTRIBUTING.md)
for the local validation commands and check scope.

Application code is Rust/WASM. Nix builds environments and tools; shell is for
setup infrastructure. Do not introduce authored JavaScript or Python.

Personal node configuration and credentials belong outside the checkout under
the configured home-directory roots. Never read or print credential contents.
Preserve unrelated user changes when staging and committing.
