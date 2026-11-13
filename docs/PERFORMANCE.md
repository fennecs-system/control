# Performance checks

## Local compaction selection (2026-11-13)

Compared merged main `b744ee2` with the local-compaction feature using the same
release `conversation::tests::continuity_profile` fixture, Linux aarch64 / Rust
1.99.0, pinned offline dependencies, one CPU, 512 MiB and no swap. Six alternating
pairs followed warmup. Compaction remained disabled for this comparison; rendered
prompt sizes were identical on both revisions.

| Retained turns | Baseline → feature median | Prompt bytes |
| --- | ---: | ---: |
| 15 | 33.609 → 32.917 µs | 17,146 |
| 150 | 56.501 → 51.167 µs | 24,888 |

These runs show no disabled-path regression in this fixture. There is no intended
speed improvement on that path; compiler layout and measurement effects can
account for the observed difference. This is not an end-to-end inference result.

The explicit `compaction::tests::selection_profile` fixture measures enabled,
cached selection separately. It revalidates source hashes before reusing a view:

| Source entries | Cached selection median | Selected summary nodes | Mock cold calls |
| --- | ---: | ---: | ---: |
| 32 | 10.911 µs | 4 | 12 |
| 256 | 81.421 µs | 7 | 121 |
| 1,024 | 318.403 µs | 9 | 503 |

The source-validation pass is linear in the bounded snapshot, while the selected
summary cut is logarithmic. Flamegraphs show source rendering and hashing as the
main enabled-path work. Snapshot limits are 4096 entries / 1 MiB; this fixture
uses short synthetic records, not maximum-sized entries. All model replies are
mocked. Four-entry leaves produce many cold calls; operators can increase the
maximum leaf size within their model's input budget, trading fewer requests for
larger summaries. Warm persistent-cache reuse is covered separately by a test
that forbids any repeated model call. No Qwen fidelity, inference latency or
hardware-fit claim follows from these measurements.

Separate 499 Hz user-space CPU profiles cover baseline, disabled feature and
enabled selection. Artifacts and logs are retained under
`.control/performance/local-compaction-20261113/` and the isolated profiling
scratch directory. CPU flamegraphs exclude network/disk wait time. Cgroup memory
events stayed zero, including OOM kills. The optional inference image was built
and its `--help`/`--version` entry points smoke-tested without weights; the runtime
reports llama.cpp 0.7.0, build 11669, commit `620d222`.

The library target's existing `terminal::profiling::render_profile` ran three
alternating pairs at the same runtime resource limits. For 10 / 100 / 500 messages,
baseline → feature medians were 60.799 → 61.293 µs, 138.123 → 139.327 µs and
474.670 → 486.577 µs (+0.8%, +0.9%, +2.5%). All rendered-buffer hashes matched.
These small increases are reported rather than presented as a speed gain; the
transcript renderer is unchanged and the new inspector command is not exercised
by this fixture. A discarded binary-target probe selected zero tests; only the
verified library-target runs contribute to these figures.

## Approval publication ordering (2026-11-13)

Compared merged main `93d189d` with the approval-publication slice, using the
existing synthetic release fixture on Linux aarch64 / Rust 1.99.0 with pinned
offline dependencies. Six alternating pairs after warmup ran on one CPU with
512 MiB and no swap. Both revisions perform one dispatcher notification per
admission; the revised fixture supplies that notification after its synthetic
successful acknowledgement because publication no longer wakes by itself.

| Retained completed runs | Baseline → revised median | Change |
| --- | ---: | ---: |
| 16 | 786.5 → 941 ns | +154.5 ns (+19.6%) |
| 1,024 | 10.719 → 9.963 µs | −7.1% |
| 20,001 | 408.374 → 375.511 µs | −8.0% |

Typed `Busy` versus `Rejected` admission results remove duplicate authorization
and capacity checks while preserving denial-before-capacity semantics. Larger
ledgers improve, but the small fixture retains a fixed sub-microsecond overhead
from the preparation/publication split; this is not a claim of zero regression.
The fixture excludes checkpoint I/O, request grouping and bot reconciliation;
it does not measure end-to-end approval latency or the new status projection.

Actual CPU-clock flamegraphs contain 2,514 / 2,175 samples. Memory-event counters
did not increase during the measured run; no OOM occurred. Scripts, timing logs,
raw stacks, SVGs and binary hashes are retained locally under
`.control/performance/approval-publication-20261113/`. Tests separately verify
checkpoint-before-wake, save rejection without wake, reload deduplication and
paused/queued/running labels. No live model calls were made.

## Approval eligibility and checkpoint reconciliation (2026-11-13)

Compared main `dda2b89` with runtime changes through `5f0babc`, with identical
profiling fixtures injected into the baseline. Linux aarch64 / Rust 1.99.0,
identical release flags and offline pinned dependencies; one CPU, 512 MiB and
no swap during measurement. No live app, model or real credential was involved.
After warmup, six alternating baseline/revised pairs measured these paths:

| Retained completed runs | Approval resume, baseline → revised | Change |
| --- | ---: | ---: |
| 16 | 772 → 724 ns | −6.2% |
| 1,024 | 10.222 → 10.346 µs | +1.2% |
| 20,001 | 361.957 → 362.823 µs | +0.2% |

The approval fixture includes eligibility, host authorization/capacity, queue
construction, note append and dispatcher notification, then resets the appended
records. Each process retains ten 100-operation batches after discarding the
first. Larger-history process medians overlap; no broad speedup is claimed for
moving the existing selection rules behind a borrowed interface.

Normal saves use the existing 31-sample durable-save fixture, rotating its four
stages. Median process medians for full serialization/publication are below.
Tmpfs removes durable-storage latency but does **not** test durability.

| Runs / finished handoffs | Disk save, baseline → revised (ms) | Tmpfs save, baseline → revised (µs) |
| --- | ---: | ---: |
| 1 / no | 2.306 → 2.173 | 40.980 → 40.188 |
| 1 / yes | 1.724 → 2.131 | 40.084 → 39.917 |
| 100 / no | 1.504 → 1.803 | 110.730 → 107.813 |
| 100 / yes | 1.584 → 1.758 | 151.251 → 153.229 |
| 1,000 / no | 2.963 → 2.932 | 727.563 → 733.459 |
| 1,000 / yes | 3.379 → 3.388 | 1,104.772 → 1,122.418 |

Normal successful saves gain no extra filesystem calls. Tmpfs changes range from
−2.6% to +1.6%; disk results are noisier, including case medians up to +23.6%.
These measurements do not prove unchanged durable-save latency. Reconciliation
performs its bounded-buffer comparison and additional syncs only on uncertainty.

Checkpoint loading now confirms file/directory durability before recovery. For a
74,208-byte checkpoint, six process medians (50 retained loads after warmup)
give **28.865 → 29.698 µs**, about +0.833 µs (+2.9%). This measures repeated
warm, clean-file loads, not cold startup or degraded storage; sync latency on
other filesystems can be higher. The confirmation is a correctness cost at
startup, not an added sync on ordinary chat/tool reads.

Actual CPU-clock samples/flamegraphs cover approval admission (2,212 / 2,211
samples, 1,000-operation batches) and save stages (65 / 76 sparse samples).
User-space CPU samples do not measure blocked fsync time. No memory-pressure,
limit or OOM events occurred. Scripts, timing logs, raw stacks, SVGs, binary
hashes and resource settings remain locally in
`.control/performance/approval-checkpoint-20261113/`. Fault tests establish the
new recovery/fencing behaviour separately; this is not a game or model-throughput
comparison and does not establish out-of-lock or async persistence safety.

## Shared continuation constructor (2026-11-13)

Compared deployed main `6579846` with constructor slice `16bc49a`, injecting the
same ignored `chat::tests::profile_continuation_admission` fixture into both
snapshots. Both used identical release flags with debug symbols, pinned offline
dependencies and Linux aarch64 / Rust 1.99.0. Timing used one CPU, 512 MiB and no
swap in an isolated helper, with no live application/model calls. The fixture
queues a continuation from captured settings, then removes that run to preserve
the retained ledger. It includes authorization/capacity checks, queue-order
scan, construction and dispatcher notification; it excludes persistence, real
workers, web/TUI rendering and context-history hydration.

After warmup, six alternating baseline/revised pairs each retained ten batches
of 100 operations, discarding the first batch. The table reports the median of
the six process medians; retained records are completed runs.

| Retained runs | Baseline | Revised | Change |
| --- | ---: | ---: | ---: |
| 16 | 277.5 ns | 236.5 ns | −14.8% |
| 1,024 | 1.586 µs | 1.502 µs | −5.3% |
| 20,001 | 100.874 µs | 99.548 µs | −1.3% |

All six short-ledger pairs improved. The large-ledger process medians overlap
(baseline 98.752–102.170 µs; revised 99.066–103.078 µs), so the largest case is
effectively unchanged at this precision; ledger scans still dominate. This is a
small admission-path saving, not a model-throughput or whole-application speedup.
No new memory-limit or OOM events occurred. Real `perf record` CPU-clock samples
(577 / 565, using 1,000-operation batches) and before/after flamegraphs accompany
the timings under `.control/performance/continuation-construction-20261113/`, with
scripts, resource limits, binary hashes and raw stacks.

## Classified checkpoint boundary (2026-11-13)

Compared test-layout baseline `bbdd73c` with checkpoint adapter `9b2765a`, using
the existing `storage::tests::profile_durable_save_stages` release fixture.
Both binaries used identical flags (debug symbols retained), Linux aarch64,
Rust 1.99.0, one CPU and 512 MiB without swap in an isolated helper. Live bots,
models and application state were not involved. Each fixture rotates serialization,
overwrite/fsync, atomic replacement and complete save, retaining 31 samples per
stage and case. Complete-save medians below are medians across three alternating
disk pairs after warmup, and six alternating tmpfs pairs respectively.

| Retained runs / finished handoffs | Disk save, baseline → revised (ms) | Tmpfs save, baseline → revised (µs) |
| --- | ---: | ---: |
| 1 / no | 2.144 → 2.068 | 43.29 → 44.00 |
| 1 / yes | 2.709 → 2.871 | 44.52 → 41.83 |
| 100 / no | 1.517 → 1.524 | 114.98 → 115.90 |
| 100 / yes | 2.199 → 2.468 | 163.11 → 162.42 |
| 1,000 / no | 3.167 → 3.485 | 754.73 → 745.92 |
| 1,000 / yes | 3.619 → 3.748 | 1,158.84 → 1,144.61 |

Disk results are noisy: individual process medians span 1.16–4.02 ms before and
1.12–4.15 ms after. Some case medians rise up to 12%; this small sample cannot
prove unchanged durable-save latency. The tmpfs control removes durable storage
latency while retaining serialization and filesystem calls: changes range from
−6.0% to +1.6%, with no consistent CPU overhead detected. Tmpfs does **not** test
durability. No speed improvement is claimed for adding error classification.

Actual `perf record` CPU-clock stacks and flamegraphs were collected for both disk
fixtures (72 / 115 samples). These sparse user-space samples are a sanity check,
not evidence about blocked fsync time or a statistically precise CPU comparison.
No new memory-limit or OOM events occurred. Logs, scripts, resource settings,
binary hashes, raw samples and SVGs remain locally under
`.control/performance/checkpoint-outcomes-20261113/`. This does not measure provider
throughput, web/TUI rendering or persistence moved outside the lock.

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

## Typed room notification publication (2026-11-13)

Compared main `457a117` with the typed publication operation using an identical
`profile_room_publication` fixture in both source snapshots. The fixture admits
and publishes one notification through reconciliation, verifies its receipt and
run count, then resets the touched records for the next iteration. Checkpoint
I/O is a counted no-op and no worker/model is started. Timings include planning,
run construction, mutation and fixture reset, but exclude fsync, active dispatcher
delivery and real application workload latency.

Linux ARM64/rustc 1.99.0, release with debug symbols, one pinned virtual CPU,
512 MiB and no swap. Each fixture uses ten batches of 100 iterations, discards
the first batch and reports the remaining median. One warmup fixture per revision
precedes six alternating pairs; the table shows medians of their six medians.

| Retained runs | Before | After | Change |
| --- | ---: | ---: | ---: |
| 16 | 938 ns | 951 ns | +1.4% |
| 1,024 | 31.219 µs | 30.751 µs | -1.5% |
| 20,001 | 1.050 ms | 0.985 ms | -6.2% |

The smaller cases are roughly unchanged. The largest fixture is lower in all
six pairs; this is a result for this in-memory publication path, not a general
server or TUI speedup. The driver now checks capacity once under exclusive
ownership rather than again inside `Runtime::enqueue_for`, and constructs the
run directly from captured settings. Retained-run index/queue-order scans remain
linear. No allocation or resident-memory improvement is established.

Real `perf` captures use `cpu-clock:u`, 499 Hz and DWARF stacks: 6,628 baseline
samples and 6,570 revised samples, rendered with Inferno. Profiling/symbolization
use 2 GiB, outside the timing runs. During the measured pairs, memory-limit and
OOM counters do not increase (the max counter was already nine after setup and
warmups). Raw logs, environment/counters, scripts, perf data and flamegraphs are
retained under `.control/performance/room-effects-20261113/` (ignored locally).
These measurements and fake-checkpoint tests do not cover ambiguous real save
outcomes; see [state boundaries](STATE-BOUNDARIES.md).

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

The common `watches::tests::collector_profile` fixture uses only the old runner API and
can be copied unchanged into the baseline. Run it only inside an isolated Linux
container with a private home and writable `/workspace`, without credentials:

```sh
rustc --edition=2024 --test -O -C debuginfo=1 tools/service-runner/main.rs -o /tmp/runner-tests
/tmp/runner-tests watches::tests::collector_profile --ignored --exact --nocapture
```

For longer sampling, set `CONTROL_WATCH_PROFILE_BATCHES=128` and apply the
`perf record`/Inferno procedure below. Functional regressions separately verify
that diagnostic floods admit no turns, a selected notification releases an
event-only wait once, and published notification receipts suppress an extra turn.

## Shared client and terminal build boundary (2026-11-10)

Compared main `52bff4c` with the client/terminal module extraction. The identical
offline Ratatui `TestBackend` fixture constructs a transcript and draws a 100×30
frame. Six alternating pairs each discard one warmup batch and retain nine
batches of 100 frames. Linux ARM64 timings use one virtual CPU, 512 MiB and no
swap, with the same release toolchain. Medians of the six fixture medians:

| Messages | Before | After | Change |
| --- | ---: | ---: | ---: |
| 10 | 63.299 µs | 63.366 µs | +0.1% |
| 100 | 143.474 µs | 146.010 µs | +1.8% |
| 500 | 499.106 µs | 503.656 µs | +0.9% |

Visible frame hashes match. These small increases are not a rendering speedup or
a guarantee of zero overhead. The fixture measures transcript construction and
in-memory terminal layout/drawing, not actual terminal I/O or interactive latency.

Stripped Linux executables are 10,820,376 bytes for both default servers and
10,427,072 bytes for the new server-only variant (-3.6%). The standalone terminal
grows 3,807,632 → 3,873,168 bytes (+64 KiB); text/data totals grow 8,592 bytes.
This is a recorded cost of the shared library boundary, not a size improvement.
The server-only normal dependency graph contains neither ratatui nor crossterm.

101 alternating startup pairs, after three warmups, give 473.585/469.584 µs for
`--version` and 463.209/465.126 µs for MCP startup to stdin EOF: effectively
unchanged at this scale. In a separate comparison, baseline/server-only version
startup is 451.710/447.668 µs. Initial grouped startup results varied substantially;
the alternating comparisons avoid that order effect. Standalone TUI connect/check
uses three warmups and 31 launches per variant against a scratch loopback node:
152.601/152.603 ms, and 152.310 ms against the server-only node. That check does
not measure interactive rendering. Fresh-node readiness waits for HTTP health,
not merely an open socket; no production retry or timeout policy was changed.

Real terminal flamegraphs capture 3,340/3,279 `cpu-clock:u` samples at 499 Hz with
DWARF call stacks and function-level Inferno conversion. Profiling/symbolization
uses 2 GiB; compilation uses 4 GiB. Timing runs record no OOM events. No models,
live node state or credentials are used. The lean Nix package also passes local
health, shared MCP read/denial and both terminal-entry-point checks on a scratch
node with zero model runs.

Reproduce the after fixture with `cargo test --release --locked --lib
terminal::profiling::render_profile -- --ignored --exact --nocapture` in the pinned
environment. For before/after work, copy the same fixture into the baseline's
terminal module. The server-only build is `cargo build --release --locked
--no-default-features --bin control`, or `nix build .#control-server`.

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
`terminal::tests::live_rust_benchmarks_use_mcp_and_compare_html_escaping` library test runs
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

## Terminal command discovery (2026-11-13)

Compared merged main `93d189d` with the terminal-only command catalogue/help
changes using the existing release render fixture. Linux aarch64 / Rust 1.99.0,
pinned offline dependencies, identical release flags and six alternating pairs
after warmup; one CPU, 512 MiB and no swap during measurement.

| Transcript messages | Baseline → revised render median | Change |
| --- | ---: | ---: |
| 10 | 62.750 → 63.841 µs | +1.7% |
| 100 | 141.492 → 143.214 µs | +1.2% |
| 500 | 495.203 → 492.831 µs | −0.5% |

Process-median ranges overlap in every case and rendered-buffer hashes match.
This is evidence against a material transcript-render regression in this fixture,
not proof of unchanged latency on every device. It does not exercise popup layout
or network polling. A separate revised-only fixture measures discovery plus
highlight construction at **201–267 ns** for `/`, `/con`, `/context` and `/improve`
and **38 ns** for ordinary text. That is not full-frame or keyboard latency.

Actual render flamegraphs contain 1,032 / 1,042 CPU-clock samples. Memory-event
counters did not increase during measurement and no OOM occurred. Timing logs,
raw stacks, SVGs, scripts and binary hashes remain locally under
`.control/performance/tui-commands-20261113/`. An interactive local-client smoke
check completed `/con` with Tab and opened `/help` without submitting a model
prompt; all existing paused bot/run state was preserved.

## Model/compaction series (2026-11-13)

Compared deployed `b744ee2` with the series through chat model selection using
the existing release fixtures, Linux aarch64 and the pinned offline toolchain.
Six alternating history pairs and three TUI pairs followed warmup. Measurements
used one pinned CPU, 512 MiB and no swap; the external Qwen experiment used
different CPU cores on the same host. Treat host pressure and compiler layout as
possible noise, particularly for code paths unchanged with compaction disabled.

| Fixture | Baseline → revised median |
| --- | ---: |
| Managed context, 15 turns, compaction off | 37.033 → 35.971 µs |
| Managed context, 150 turns, compaction off | 62.736 → 57.493 µs |
| TUI render, 10 messages | 83.960 → 79.354 µs |
| TUI render, 100 messages | 179.193 → 180.981 µs |
| TUI render, 500 messages | 618.735 → 632.162 µs |

Prompt sizes match at 17146/24888 bytes; every rendered-buffer hash matches.
The largest TUI median increase is 2.2%, with overlapping sample distributions.
No material regression is established by these fixtures; they do not measure
network polling, end-to-end inference or every workload. The revised enabled
selection fixture took 7.702/43.955/157.545 µs for 32/256/1024 entries, selecting
4/7/9 tree nodes. Its model calls are deterministic mocks, not Qwen timings.
Covered-prefix validation still scans retained source; this is not constant-time
history access. Cold summary generation and disk cache size are separate costs.

CPU-clock flamegraphs were captured for baseline/revised history and revised
enabled selection. Memory-event counters remained zero, including OOM. Local
logs, raw profiling data and SVGs are retained under
`/private/tmp/control-boundary-profile/compaction-series/`; copied logs/SVGs are
under `.control/performance/compaction-series-20261113/`. The profiling helper is
stopped. These fixtures profile the host/core path; the inference container is an
optional external service and its RAM is reported separately in
[LOCAL-MODELS.md](LOCAL-MODELS.md).

