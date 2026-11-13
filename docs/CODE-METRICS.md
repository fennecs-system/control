# Codebase size and complexity

The separate **Codebase metrics (advisory)** workflow compares the PR base and head
with the same pinned tools. On pushes to main it compares the pushed range; a manual
run compares the previous commit. It publishes a job summary and 14-day raw artifacts.
It does not compile Control, call models, post comments, or require provider credentials.
There are no LOC, complexity or duplication merge thresholds. Tool failures still fail
the metrics job so a broken report cannot silently become a clean bill of health.

The aim is to identify repeated implementations and hard-to-navigate boundaries,
not minimize characters. Features, useful tests, explicit typed contracts and measured
performance improvements can justify growth. Review the affected functions and their
callers before consolidating them; a shared abstraction that hides ownership or policy
can be worse than a little repetition.

## Measurements

- [Tokei](https://github.com/XAMPPRocky/tokei) counts code, comments and blanks by
  language/file. The summary separates server/root sources, shared client, terminal,
  library crates, web, tools and dedicated tests. Inline tests remain included in
  each boundary's LOC; this is not a production-only LOC claim.
- [Mozilla rust-code-analysis](https://mozilla.github.io/rust-code-analysis/metrics.html)
  supplies per-function cyclomatic and cognitive complexity. Cyclomatic measures
  branching paths, while cognitive complexity considers nesting/readability costs.
  The summary ranks named functions outside recognized test scopes; all scopes and
  anonymous closures remain in the raw analyzer output. Function sums include nested
  closures, so summing all function scores would double-count work.
- A small Rust reporter uses Syn source spans to recognize direct `#[cfg(test)]`
  modules/functions/impls, `#[test]` functions, dedicated `tests/` directories and
  conventional `tests.rs` files (including unannotated fixture helpers). File naming
  is a convention-based label; it does not resolve external module declarations.
  It does not evaluate arbitrary `cfg` expressions or expand macros. Complex conditional
  test scopes and macro-generated code require manual inspection. These are source
  metrics, not executable path coverage or Rust compiler complexity.
- [jscpd v5](https://github.com/kucherenko/jscpd/blob/v5.4.3/docs/rust.md) is a native
  Rust clone detector, installed from a versioned, SHA-256-pinned release archive.
  It scans Rust with comments/whitespace ignored and a conservative minimum of
  100 tokens and 12 lines per match. Raw JSON includes both locations. Clone counts
  include tests and repetitive boilerplate; they are investigation candidates, not
  evidence of redundant features. Renamed/semantic clone detection is not enabled.

Only tracked files from **git archive** snapshots are scanned, rather than the working
checkout. This excludes this repository's untracked generated outputs, dependencies,
scratch files and operator configuration. Tracked generated or vendor code would need
an explicit scope decision; Git does not identify it automatically. The analyzer rejects
symlinks and build output directories. Authored setup/config/docs remain in the
all-language raw LOC report. There is no automatic classification of arbitrary code
as boilerplate.

Halstead vocabulary/volume and maintainability estimates are available in the raw
Mozilla output, but are not combined into an opaque health score. Token entropy,
gzip size and shorter identifiers can reward repetition or code golf; none establishes
that an agent can understand or modify the program more reliably. Agent usefulness
needs evidence such as less repeated investigation, narrower edits and successful
tests, alongside these source measurements.

## Run locally

```sh
nix build .#code-metrics --out-link /tmp/control-code-metrics
mkdir -p /tmp/control-metrics-source /tmp/control-metrics-report
git archive HEAD | tar -x -C /tmp/control-metrics-source
/tmp/control-code-metrics/bin/control-code-metrics scan /tmp/control-metrics-source /tmp/control-metrics-report
```

Use fresh empty directories. To compare, scan another revision into a second output
directory with the **same analyzer**, then run:

```sh
/tmp/control-code-metrics/bin/control-code-metrics compare /tmp/base-report/summary.json /tmp/head-report/summary.json
```

Toolchain/compiler caches are independent of the application. Nix caches the tools
and reporter; only this small standalone Rust tool builds when its sources change.
The production dependency graph and binary contain none of this tooling. Upstream
tools are fixed by `flake.lock` and the jscpd archive hashes; Dependabot covers the
reporter's separate Cargo lockfile and the workflow action pins.

Initial local baseline on `457a117` (2026-11-13): 57,978 Rust code lines across
146 files, 2,399 named functions, 930 functions in recognized test scopes and
three clone pairs at these conservative settings. All three pairs are test code;
this does not establish an absence of semantic duplication. The largest reported
hotspots include worker tool dispatch, bot reconciliation and worker execution,
which makes the ongoing typed-operation separation a useful place to investigate.
One native macOS scan took 9.9 seconds and produced about 12 MiB of raw reports;
that is a local observation, not a CI timing guarantee. CI scans two revisions;
tool fetching/building and artifact upload add separate overhead.
