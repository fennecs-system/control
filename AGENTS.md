# Repository workflow

Use a feature branch for changes. Run the relevant local checks before pushing
and report their results in the pull request. The maintainer manages PR merges;
agents must not merge PRs. Keep `main` buildable; fix a failed main check before
starting another feature. Deploy a merged revision when requested. See [CONTRIBUTING.md](CONTRIBUTING.md)
for the local validation commands and check scope.

Only after the merged application has redeployed successfully and health checks
pass, clean up obsolete clean worktrees, verified merged branches and scratch files
from that work, then prune stale refs. Check branch tips before deleting them.
Preserve dirty worktrees, unmerged work, active resources, credentials, required
helpers and current verification/deployment logs. If deployment fails, retain the
worktrees and artifacts needed to diagnose or recover it.

Application code is Rust/WASM. Nix builds environments and tools; shell is for
setup infrastructure. Do not introduce authored JavaScript or Python.
Non-Rust third-party tooling is allowed only inside tool/agent containers;
Control and authored integrations remain Rust/Haskell/Lean/WASM/Nix. An upstream
container dependency does not authorize a host Node service or JavaScript adapter.
Agent-generated programs outside this repository may use other languages inside
explicitly granted tool containers. Keep task-specific servers, game adapters
and their configuration out of Control's maintained source; provide generic
resource, transport and tool capabilities instead.

Personal node configuration and credentials belong outside the checkout under
the configured home-directory roots. Never read or print credential contents.
Preserve unrelated user changes when staging and committing.
