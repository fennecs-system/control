# Codebase size and complexity

The **Codebase metrics (advisory)** workflow (`.github/workflows/code-metrics.yml`)
scans two revisions with the same pinned tools: a pull request's base and head, a
push's previous and new tip, or, for a manual run, the previous commit and the
current one. It writes a job summary and keeps the raw reports as an artifact for
14 days. It does not compile Control, call models, post comments or need provider
credentials.

There are no size, complexity or duplication merge thresholds. A tool failure
still fails the job, so a broken report cannot look like a clean one.

The aim is to find repeated implementations and hard-to-navigate boundaries, not
to minimize characters. Features, useful tests, explicit typed contracts and
measured performance work can justify growth. Read the affected functions and
their callers before consolidating: a shared abstraction that hides ownership or
policy can be worse than a little repetition.

## Measurements

- **Lines of code:** [Tokei](https://github.com/XAMPPRocky/tokei) counts code,
  comments and blanks per language and file. The summary splits server/root
  sources, shared client, terminal, library crates, web, tools and dedicated
  tests. Inline tests stay in each area's count, so this is not a production-only
  figure.
- **Complexity:** [Mozilla rust-code-analysis](https://mozilla.github.io/rust-code-analysis/metrics.html)
  gives per-function cyclomatic complexity (branching paths) and cognitive
  complexity (nesting and readability cost). The summary ranks named functions
  outside test scopes; the raw output keeps every scope and closure. A function's
  score includes its nested closures, so summing all scores double-counts.
- **Test scopes:** a small Rust reporter uses Syn spans to recognize
  `#[cfg(test)]` modules, functions and impls, `#[test]` functions, `tests/`
  directories and `tests.rs` files. File naming is a convention, not resolved
  module structure. It does not evaluate arbitrary `cfg` expressions or expand
  macros, so conditional test code and macro output need manual review.
- **Clones:** [jscpd v5](https://github.com/kucherenko/jscpd/blob/v5.4.3/docs/rust.md),
  installed from a SHA-256-pinned release archive, finds Rust clones of at least
  100 tokens and 12 lines, ignoring comments and whitespace. Clone counts include
  tests and boilerplate; treat them as leads, not proof of redundant features.
  Renamed or semantic clones are not detected.

These are source metrics, not execution coverage. Halstead and maintainability
values are in the raw Mozilla output but are not combined into a single health
score. Token entropy, compressed size or shorter identifiers can reward repetition
or code golf and say nothing about whether an agent can work on the code; that
needs evidence such as fewer repeated investigations, narrower edits and passing
tests.

Only files tracked by Git are scanned, from a `git archive` snapshot, so untracked
outputs, dependencies, scratch files and operator configuration are excluded.
Tracked generated or vendored code is scanned like any other file. The analyzer
rejects symlinks and build output directories.

## Run locally

```sh
nix build .#code-metrics --out-link /tmp/control-code-metrics
mkdir -p /tmp/control-metrics-source /tmp/control-metrics-report
git archive HEAD | tar -x -C /tmp/control-metrics-source
/tmp/control-code-metrics/bin/control-code-metrics scan /tmp/control-metrics-source /tmp/control-metrics-report
```

Use fresh, empty directories. To compare, scan another revision into a second
report directory with the same analyzer build, then:

```sh
/tmp/control-code-metrics/bin/control-code-metrics compare /tmp/base-report/summary.json /tmp/head-report/summary.json
```

The tooling is a standalone Rust crate (`tools/code-metrics`) and is not part of
Control's dependency graph or binary. Upstream tools are pinned by `flake.lock`
and the jscpd archive hashes; Dependabot covers the reporter's Cargo lockfile and
the workflow's action pins. Changes to the reporter should pass its locked tests
and strict Clippy with `--manifest-path tools/code-metrics/Cargo.toml`, plus a real
snapshot comparison.
