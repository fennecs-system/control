# Performance checks

## Small-machine efficiency goal

Control's core, tools and TUI should have sufficiently low overhead to be
plausible on a tiny machine. Pi Zero-class hardware illustrates that resource
class; it is not a required board, instruction set or 512 MiB release gate.
The website has a separate deployment and performance budget. The goal is an
efficient architecture, rather than certification for a particular device.

Measure cold and warm startup, peak/idle RSS, idle CPU, tool dispatch, context
construction and TUI input/render latency, including tail latency and growing
history. Record architecture, storage, concurrency and swap use. Model execution
and requested workload resources have separate budgets; report Control's own
overhead explicitly. Avoid extra timers, duplicated state, repeated history
hydration and dependencies in the core. Keep web assets and optional adapters
separable through the typed state/effect boundaries; the current all-in-one
binary is not evidence of a website-free minimal build.

Keep state and typed effects independent of execution backends. Treat Docker as
one backend adapter; alternative runtimes should implement the same
capability, ownership and lifecycle contracts. Measure Control's own overhead
separately from each backend's startup, resident services and workload costs.
Prefer event-driven idle behavior, bounded active projections and journal-backed
history over polling, whole-state copies or repeated reconstruction.

Use reproducible baselines to catch relative regressions and justify overhead.
Constrained-container and hardware checks provide additional evidence, without
making a named board a prerequisite. The 512 MiB/no-swap experiment below is
one measurement condition, not a product support promise or mandatory budget.

## One-shot room admission boundary (2026-11-13)

Compared main `52bff4c` with the borrowed application planner using the same
`profile_idle_and_waiting_room_reconciliation` fixture in both source snapshots.
The fixture now accepts `CONTROL_ROOM_PROFILE_ROUNDS` for the armed case; its
default remains 10. Timings used 100 iterations per batch, nine batches per
fixture, one warmup fixture per revision and six alternating pairs. Linux ARM64,
rustc 1.99.0, release with debug symbols, one pinned virtual CPU, 512 MiB and no
swap were held constant. Compiler builds and stack symbolization used higher
memory limits, outside the timing runs.

With 20,001 retained runs and one armed, waiting subscription, the median of the
six fixture medians was 766,740.5 ns before and 762,609.5 ns after (-0.54%). The
ranges were 720,569–814,141 ns and 691,706–805,498 ns. Treat this as roughly
unchanged performance, not a speedup. The unarmed fast path remained at the
fixture's few-nanosecond measurement floor. No model, container lifecycle or
checkpoint I/O occurs inside this waiting-path fixture; its save callback fails
the test if called. It does not measure queue publication/fsync, interactive TUI,
web latency or overall swarm throughput.

Real `perf` captures use `cpu-clock:u`, 499 Hz and DWARF stacks: 3,229 baseline
samples and 3,464 revised samples, rendered with Inferno. The revised stacks
contain `application::room_wakes::plan`; retained-run index construction remains
in this path. This extraction does not remove its linear scan. Timing-run
`memory.events.max` increased by six while OOM/kill counters stayed zero;
the logs retain this limit pressure and do not establish a resident-memory
improvement. Raw paired logs, environment, memory counters, scripts, perf data
and flamegraphs are retained under the ignored local directory
`.control/performance/room-wakes-20261113/`.

## Watch notification transport (2026-11-09)

Compared the existing stdout collector with the notification-channel revision,
using the same isolated Linux ARM64 fixture on one virtual CPU, a 512 MiB cgroup
limit and no swap. The compatibility path keeps notifications disabled. Each run
starts eight finite processes producing 20,000 diagnostic lines each, collects
their framed output and verifies the exact source count. One warmup per revision
precedes six alternating pairs. Median wall time is 499.6 ms before and 504.3 ms
after (+0.9%); ranges are 491.6–555.3 and 490.5–520.9 ms. This fixture shows roughly
unchanged throughput within its observed variation, not a general regression bound.

Separate real `perf` captures use `cpu-clock:u`, 499 Hz and DWARF stacks: 169
baseline samples and 164 revised samples, converted with Inferno into flamegraphs.
These short captures are coarse; they do not establish allocation or idle-CPU
bounds. Enabling notifications adds a descriptor and bounded input buffer to the
existing collector thread rather than adding a thread per channel. The std-only
runner gains no dependency; model execution and task-resource costs are excluded.

The common `watches::collector_profile` fixture uses only the old runner API and
can be copied unchanged into the baseline. Run it only inside an isolated Linux
container with a private home and writable `/workspace`, without credentials:

```sh
rustc --edition=2024 --test -O -C debuginfo=1 tools/service-runner/main.rs -o /tmp/runner-tests
/tmp/runner-tests watches::collector_profile --ignored --exact --nocapture
```

For longer sampling, set `CONTROL_WATCH_PROFILE_BATCHES=128` and apply the
`perf record`/Inferno procedure below. Functional regressions separately verify
that diagnostic floods admit no turns, a selected notification releases an
event-only wait once, and published notification receipts suppress an extra turn.

## Directed-message application boundary (2026-11-10)

Compared main `856be99` with the typed state/effect extraction, using the unchanged
`agent_message::profile::routing_profile` and `performance::hot_paths` fixtures.
Both builds use the same lockfile versions, Linux ARM64 toolchain and embedded
WASM bundle. Six alternating pairs run on one virtual CPU under 512 MiB with
swap disabled. Routing discards one warmup batch and retains nine batches of
20,000 calls per size. These are medians of the six fixture medians:

| Chats/runs | Before | After | Change |
| --- | ---: | ---: | ---: |
| 16 | 0.923 µs | 0.919 µs | -0.4% |
| 64 | 1.184 µs | 1.112 µs | -6.1% |
| 1,024 | 4.038 µs | 3.475 µs | -13.9% |

The small case is effectively unchanged. This is the existing downward-message
path, including in-memory construction/reset, result encoding and dispatcher
notification, with fake archive/save effects. It excludes actual fsync, peer wake
reconciliation, provider work and async scheduling. Bounded receipt retention no
longer clones the cache on every call; the new constructor directly inherits the
recipient's settings. Treat these as CPU fixture results, not whole-swarm gains.

The existing transcript-rendering fixture measures 10, 100 and 500 messages with
0, 1,000 and 5,000 unrelated runs. Before/after medians are 43.131/42.102 µs,
498.231/476.081 µs and 2.418/2.359 ms. Output byte counts and SHA-256 hashes match
in every pair. The small decreases do not establish a general rendering speedup
from moving record types; the important check is no observed regression here.

Real `perf` sampling runs are separate from timing: `cpu-clock:u`, 499 Hz,
DWARF stacks, then `perf script --no-inline`, Inferno collapse and flamegraph.
Routing captures 1,202/1,125 samples and web rendering 141/136 (a coarse web view).
The profiler/symbolizer receives 2 GiB with swap disabled; the timing limit remains
512 MiB. The initial inline-symbolization session at 512 MiB recorded one OOM
kill and failed helper reads after the timed fixtures had finished. Final graphs
use function-level stacks and the larger profiling allowance. Only the isolated offline profiler receives
PERFMON; production services receive no profiling privileges.

Release compilation uses a separate 4 GiB allowance; runtime timings begin only
after builds finish. No models, private node state or credentials are used.
Blocking/Tokio conformance tests exercise the same message transition, but Tokio
is not a production dependency or a measured speed improvement. TUI presentation
is unchanged; its eventual crate extraction still needs dedicated interactive
rendering/startup measurements. No Pi-board or whole-server latency claim follows.

Reproduce with identical source fixtures and separate executables:

```sh
CONTROL_PROFILE_ROUNDS=20000 cargo test --release --locked --bin control \
  agent_message::profile::routing_profile -- --ignored --exact --nocapture
cargo test --release --locked --bin control \
  performance::hot_paths -- --ignored --exact --nocapture
```

## Peer coordination review (2026-11-09)

Compared the reviewed peer-delivery patch with `abc93d4`, using separate optimized
Linux ARM64 build directories, identical Rust fixtures and a two-CPU offline
container. Six alternating pairs each discard one warmup batch and retain nine
batches of 10,000 downward messages. These are medians of the six batch medians:

| Retained chats/runs | Baseline routing | Reviewed routing | Change |
| --- | ---: | ---: | ---: |
| 16 | 1.356 µs | 0.962 µs | -29.1% |
| 64 | 1.673 µs | 1.246 µs | -25.6% |
| 1,024 | 5.105 µs | 4.160 µs | -18.5% |

The first version regressed the 16-chat fixture by 6.2%. Borrowing the bounded
title and truncating the formatted objective at a UTF-8 boundary removes two
allocations without changing the 80/2,000-character limits. The fixture includes
in-memory message/queue construction and reset, excluding filesystem durability,
peer wake reconciliation, provider execution and fixture setup. It compares the
existing downward path; it does not claim these numbers for new peer delivery.
Room guidance is 1,110 → 1,108 bytes; the message tool schema is 1,048 → 1,343 bytes.

The 600-turn HTTP/save/render check remains sensitive to this VM and storage.
Unpinned bind-mount samples gave 7.429 → 7.998 ms/turn across the five measured
stages, with contradictory paired CPU changes from -29% to +49%. Six further
pairs pinned to one virtual CPU, on container-local disk under a 512 MiB cgroup
limit with swap disabled, gave 11.117 → 10.057 ms/turn and 3.793 → 3.248 ms measured
thread CPU. Both save 601 KiB. These differing results do not establish a general
save/render speedup or a precise regression bound. Retain the noisy results and
validate durability latency in representative deployment environments before
setting numeric performance gates.

Real `perf` flamegraphs cover routing and HTTP before/after, using `cpu-clock:u`,
499 Hz and DWARF stacks. Routing captures 1,712/1,260 samples and HTTP 738/757;
profiling runs are separate from timing runs. Inspect them with the conversion
commands below; elapsed timings are not used to manufacture flamegraphs.

Real stripped production executables on container-local disk were also checked
under the 512 MiB/no-swap limit. Startup uses three warmups then 31 process
launches; peak RSS uses nine separate GNU Time samples. TUI `--check` connects to
a fresh empty server; it does not measure interactive rendering.

| Footprint/check | Baseline | Reviewed |
| --- | ---: | ---: |
| Stripped Control binary | 10,754,840 bytes | 10,754,840 bytes |
| Stripped standalone TUI | 3,807,632 bytes | 3,807,632 bytes |
| MCP startup to stdin EOF, median | 0.323 ms | 0.334 ms |
| MCP peak RSS, median | 5,112 KiB | 5,112 KiB |
| TUI connect/check, median | 162.03 ms | 161.35 ms |
| TUI connect/check peak RSS, median | 7,260 KiB | 7,300 KiB |
| Empty server RSS after requests | 21,428 KiB | 21,540 KiB |

The short `--version` startup check varies from 0.335 → 0.457 ms; do not describe
all startup paths as faster. No cgroup OOM events occurred. Server memory is one
idle observation, not a bound for an active swarm; larger histories, concurrency,
tool workloads and interactive TUI rendering need their own measurements. These
results describe Control's measured overhead, not certification for a Pi board.

Reproduce routing with the same fixture copied unchanged into the baseline:

```sh
nix develop --command cargo test --release --locked --bin control \
  agent_message::profile::routing_profile -- --ignored --exact --nocapture
```

For HTTP comparisons, create a fresh `CONTROL_STATE_DIR` before each invocation
of the 600-turn fixture below. Keep revisions in separate build directories and
alternate their execution order. Never point profiling at the running node.

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

## Chat request panel

Rechecked after rebasing onto `d665d60`. Compared with `fcc76fc`, the isolated Linux ARM release fixture renders a chat
with zero or 32 pending bot approvals. Before: one transcript containing the
forms. After: transcript plus the independently embeddable request document.
Nine batches of 5,000 calls measured 0.82 → 1.01 µs with no approvals and
24.02 → 21.79 µs with 32. The latter also reflects shorter bot-card text;
it does not isolate an architectural speedup. Request content is now outside
the conversation's scroll area. Actual browser
latency also includes the additional small request-document poll each second.
Unchanged snapshots preserve DOM nodes and active approval forms.

Real before/after `perf` flamegraphs use the same offline build image, two-CPU
limit, 499 Hz CPU sampling and DWARF stacks as the continuity comparison below.
No model calls, credentials or running-node mutations are involved. Reproduce
with `cargo test --release --locked --bin control bots::tests::requests_profile
-- --ignored --exact --nocapture`. Chrome and Firefox exercise automatic panel disclosure
for a new child request, retained collapse state/drafts and slow/retried approvals.

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
