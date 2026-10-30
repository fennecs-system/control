# Performance checks

Use synthetic state and explicit workloads before changing execution policy.
Benchmarks never load personal chat history or call a model. Timing is evidence,
not a test assertion; correctness and publication fences remain separate checks.

## Rust benchmark tool

`control.rust.bench` is available through Control's MCP server; managed workers
use `control_rust_bench`. Rebuild the worker image with setup when updating from
an older installation. Old images fail compilation explicitly rather than report
unoptimized timings. The Tools catalogue groups it with the Rust playground.

Example MCP call:

```json
{
  "name": "control.rust.bench",
  "arguments": {
    "setup": "let data: Vec<u64> = (0..1024).collect();",
    "code": "std::hint::black_box(&data).iter().copied().sum::<u64>()",
    "iterations": 1000,
    "samples": 9
  }
}
```

Setup runs once; the tool warms up for up to 100 iterations, then times batches
using `Instant`. Results include raw batch nanoseconds and min/median/max per
iteration, plus the captured image digest. Compilation and container startup are
excluded. Return the measured value and black-box inputs to resist constant
folding. Mutable setup state is shared across iterations; reset it in the measured
code when that is part of the workload. Even sample counts use the upper median.

This is optimized Rust 2024 with the standard library, no Cargo dependencies or
host files. It uses the playground's credential-free offline container, read-only
root, 768 MiB memory, one CPU, 64 PIDs and 96 MiB private temporary filesystem.
Compilation has a 12-second deadline; the whole program has three seconds. Source,
sample counts and output are bounded. The tool does not request profiling kernel
privileges. Executed code and its reported results remain untrusted evidence.

For a whole Cargo project, use the existing declared build environment and
`control_workspace_start`/status tools to run its own benchmarks. This keeps long
commands, source grants, cancellation and resource ownership in one executor.

## Reproduce Control's workloads

```sh
nix develop --command cargo test --locked --release --bin control \
  performance::hot_paths -- --ignored --exact --nocapture
nix develop --command cargo test --locked --release --bin control \
  performance::ownership_snapshots -- --ignored --exact --nocapture
```

Each workload records nine batches of ten calls. Transcript workloads include
Unicode, escaping, diffs, command output and mixed run states. Unrelated runs
precede recent chat runs. SHA-256 fingerprints captured before the refactor check
byte-identical HTML in normal tests. Browser regressions additionally exercise
live updates, design questions, folder requests, focus and retained drafts.

Observed local macOS ARM release medians on 2026-10-30:

| Synthetic workload | Before | After |
| --- | ---: | ---: |
| 10 messages | 176 µs | 130 µs |
| 100 messages, 1,000 older unrelated runs | 1.27 ms | 0.74 ms |
| 500 messages, 5,000 older unrelated runs | 11.85 ms | 3.12 ms |
| 500 attempt ownership snapshots with command/log buffers | 615 µs | 310 µs |

The renderer borrows typed messages, runs and activity instead of constructing
an intermediate JSON response. Request-local borrowed run indexes avoid repeated
linear searches and preserve first-match behavior. API JSON remains a separate
projection. Ownership snapshots retain identity fields without allocating copies
of command/log strings that are immediately discarded.

MCP microbenchmarks also tested an alternative HTML escaper. Even with a plain-text
fast path it regressed sparse markup: about 1.39 µs versus 0.54 µs in the same
Linux ARM image. That candidate is retained only as a benchmark fixture under
`tools/rust-analysis/`; application escaping is unchanged. The ignored
`harness::tests::live_rust_benchmarks_use_mcp_and_compare_html_escaping` test runs
the comparison through an isolated node and checks compile errors and deadlines.

These are local synthetic measurements, not provider throughput guarantees or
kernel sampling profiles. Repeat on representative histories and the same image;
CPU contention, allocators and VM scheduling affect results.

## Execution model next

At the time of this performance pass, the dispatcher ran one model at a time
node-wide. The pass did not change
admission, ordering, cancellation, mounts, durable saves or provider access.
Durable full-ledger saves and synchronous filesystem work remain deliberate
correctness boundaries; changing them needs separate crash/recovery evidence.

Concurrency should use bounded node slots while retaining per-chat ordering and
independent writable workspaces. Parent/child waits must release slots. Exact
attempt/resource ownership, stale-result fences and shutdown recovery must hold
under overlapping execution. Set resource budgets before increasing fan-out;
do not substitute model judgment for deterministic scheduling.

The subsequent [bounded execution implementation](EXECUTION.md) changes admission
and supervision separately from these rendering measurements.
