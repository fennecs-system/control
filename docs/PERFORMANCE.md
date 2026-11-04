# Performance checks

## Active sessions and durable bot handoffs

The active-session implementation was compared with `fcc76fc` using identical
600-turn HTTP/save/render fixtures, separate release build directories, the
same offline Linux ARM image and a two-CPU limit. Six pairs alternate execution
order after builds finish. No credentials, live node state or model calls are
used. Ordinary chat medians are 6.575 ms/turn before and 6.535 ms after; median
measured thread CPU across the five stages is 5.164 ms and 5.097 ms respectively.
Both save 582 KiB. Samples range from 6.481–9.955 ms before and 6.038–7.253 ms
after. This supports no observed ordinary-path regression, not a throughput
speedup claim. Host load and filesystem latency still affect these timings.

The initial prototype regressed this fixture. Optional managed state now lives
behind boxes, finished receipt mailboxes use the existing history journal, and
progress is derived from the accepted handoff instead of duplicated per attempt.
Managed admission also avoids constructing an unused legacy prompt. On this
Linux build, `Attempt`/`Run`/`Session` sizes change from 512/1104/504 bytes to
544/1080/512 bytes. These are compiler/layout observations, not wire contracts.

The context benchmark separately compares the managed task view with this
branch's legacy prompt path over the same synthetic monitoring history. It
measures construction, not provider execution or persistence. Raw history is
retained; only the projected model input is bounded. Before/after HTTP workloads,
both context paths, journal capture and durable saves have real `perf` sampling
flamegraphs (`cpu-clock:u`, 499 Hz, DWARF call stacks), rather than diagrams
inferred from elapsed times. Only the isolated profiler container is privileged.

| Monitoring exchanges | Legacy construction | Managed construction | Managed input |
| --- | ---: | ---: | ---: |
| 15 | 9.43 µs | 2.30 µs | 4,192 bytes |
| 150 | 76.55 µs | 12.76 µs | 4,193 bytes |
| 1,000 | 183.08 µs | 101.79 µs | 4,194 bytes |

Construction still indexes retained history; bounded input does not imply
constant construction time. This fixture's repeated wake text is deliberately
unhelpful, so these reductions are not a general compression ratio.

Reproduce with fresh scratch state directories; never use a running node's state:

```sh
CONTROL_STATE_DIR=/tmp/control-http-bench CONTROL_PROFILE_CONVERSATIONS=40 \
  nix develop --command cargo test --release --locked --bin control \
  session_http::tests::fifteen_turn_conversations_workload -- --ignored --exact --nocapture
CONTROL_PROFILE_PROJECTION=managed \
  nix develop --command cargo test --release --locked --bin control \
  turn_context::tests::active_context_profile -- --ignored --exact --nocapture
CONTROL_STATE_DIR=/tmp/control-save-bench \
  nix develop --command cargo test --release --locked --bin control \
  storage::tests::profile_durable_save_stages -- --ignored --exact --nocapture
nix develop --command cargo test --release --locked --bin control \
  turn_context::tests::trace_persistence_profile -- --ignored --exact --nocapture
```

The save fixture separates serialization, overwrite plus fsync, atomic replacement
plus file/directory fsync, and the complete save across 31 rotated-order samples.
It tests 1/100/1000 finished runs with and without archived handoff receipts.
Journal capture appends and syncs exact inputs off the global state lock; normal
checkpoint saves retain their existing atomic replacement and fsync contract.
On native macOS storage, 1,000 archived handoff attempts leave an 897,525-byte
checkpoint: median serialization is 1.93 ms, atomic file/directory replacement
and fsync is 9.07 ms, and complete save is 10.00 ms (p95 12.02 ms). These stages
are independently timed and their medians need not add. Overwrite plus fsync
alone is 4.14 ms; it provides weaker crash guarantees than atomic replacement.
Exact-input journal appends measure 4.08 ms median for 4,404-byte records over
nine batches of 100 durable writes, close to the overwrite/fsync floor. No fsync
was removed or deferred to obtain those results.
These synthetic checks do not establish the quality or latency of a live bot
playtest, nor do they measure a provider's private context management.

## Bot turn continuity

The continuity fix was compared with `8f170c7`, using identical synthetic
fixtures, Rust release builds with debug symbols, the same offline Linux ARM
build image and a two-CPU container limit. No credentials or model calls were
used. Nine batches of 20,000 prompt constructions include hydration of the
previous attempt's archived observations:

| Context workload | Before | After |
| --- | ---: | ---: |
| 15 turns, 16 prior observations | 8.42 µs | 35.72 µs |
| 150 turns, 16 prior observations | 26.36 µs | 61.05 µs |

This is an intentional correctness cost, not a performance improvement: later
turns now receive execution evidence and omitted user instructions. Genuine
`perf` sampling flamegraphs were captured before and after with
`cpu-clock:u`, 499 Hz and DWARF call stacks. Archived activity hydration appears
in approximately 34% of weighted after-context samples; JSON decoding overlaps
that stack. The new handoff formatting appears in approximately 16%.

The existing 600-turn HTTP/save/render workload measured 6.85 ms/turn before
and 6.47 ms after, with 582 KiB saved state in both cases. This single paired
sample suggests no regression in that path; it does not prove a speedup or
measure model throughput. It does not construct model prompts, so it cannot
hide the extra prompt cost above. Both workloads have before/after flamegraphs.

On macOS ARM, a separate release benchmark with 64 archived observations of
4,000 Unicode characters each measured 0.360 ms per prompt over nine batches
of 100 calls. Excerpt limits bound prompt output, not the journal decode;
the existing reader hydrates the entire previous record. Cold journal opening
also scans offsets and was not isolated by these warm-loop timings. Retained
user input grows with human instructions; the future summary plugin addresses
that separately.

Reproduce the synthetic prompt checks:

```sh
nix develop --command cargo test --release --locked --bin control \
  conversation::tests::continuity_profile -- --ignored --exact --nocapture
nix develop --command cargo test --release --locked --bin control \
  conversation::tests::large_observation_profile -- --ignored --exact --nocapture
```

For Linux sampling, build the test binary with `CARGO_PROFILE_RELEASE_DEBUG=1`,
then run that binary under `perf record -e cpu-clock:u -F 499
--call-graph dwarf,8192` with the same test filter. Convert `perf script` through
`inferno-collapse-perf` and `inferno-flamegraph`. Only the isolated profiling
container receives kernel profiling privileges; application workers do not.

## Core scheduling extraction

The initial `control-core` scheduling boundary uses borrowed, statically
dispatched views and retains the original FIFO, chat ownership, capacity and
attempt-fencing behavior. The application tests compare it with the original
scheduler over 2,000 deterministic mixed ledger states, including unknown saved
statuses, tied order, missing chat IDs, answered questions and delegation.

Reproduce the old/new release comparison without credentials or model calls:

```sh
nix develop --command cargo test --release --locked --bin control \
  scheduler::qa::scheduling_profile -- --ignored --exact --nocapture
```

Observed development Mac medians across three invocations; each invocation uses
nine batches of 200 calls per implementation. Long ledgers contain 5,032 runs:

| Workload | Previous | Core |
| --- | ---: | ---: |
| Open, 32 runs | 266 ns | 76 ns |
| Open, long ledger | 9.09 µs | 8.25 µs |
| One occupied slot, long ledger | 8.91 µs | 8.74 µs |
| Waiting chats, long ledger | 9.09 µs | 9.15 µs |
| Paused, long ledger | 2.95 µs | 3 ns |
| Full slots, long ledger | 2.92 µs | 2.89 µs |
| Full admission, long ledger | 3.91 µs | 3.90 µs |
| Available admission, long ledger | 3.63 µs | 3.65 µs |

Ordinary long-ledger cases are neutral within observed sampling variation;
waiting-chat samples ranged 8.96–9.16 µs before and 9.02–9.19 µs after. These
microbenchmarks do not establish an end-to-end model throughput improvement.
No additional history buffers or persistent indexes are introduced. Paused
selection now returns before examining history. Eager record projections and
early-exit counting variants were rejected after they slowed ordinary queues;
the selected implementation retains compiler-friendly full reductions.

## Rust microbenchmark tool

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
