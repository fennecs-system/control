# control-core

Pure application rules over borrowed inputs. The initial extraction contains:

- fixed node admission and parallelism limits;
- validated nonzero slot capacity;
- deterministic FIFO dispatch with per-chat ownership and explicit waiting state;
- matching of reported attempts against the current active generation.

`RunView` uses static dispatch, borrowing fields from the existing ledger. It
does not allocate an intermediate ledger, copy histories, parse JSON or acquire
locks. Query defaults follow the typed `Phase`; optimized adapters must agree
with those defaults. The application tests check that contract and compare the
new adapter with the original scheduler across 2,000 mixed ledger states.

Core has no dependencies. Transport, UI, credentials, config loading, persistence,
Docker effects and concrete provider protocols remain outside it. This first
slice does not yet extract the full application command/query service. Attempt
matching is one lifecycle predicate, not a substitute for actor authorization,
workspace validation or verified container ownership.

Run `cargo test -p control-core` independently. Application regressions and the
release comparison workload live under `scheduler::qa` in the server tests.
See `docs/PERFORMANCE.md` for the reproduction command and measurements.
