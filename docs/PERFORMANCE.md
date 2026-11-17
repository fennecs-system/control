# Performance

How Control keeps its own overhead small, how to measure it, and the current
reference numbers for each subsystem. Figures are local synthetic measurements:
they show relative cost and catch regressions; they are not provider throughput
or latency guarantees.

## Goals

Control's core, tools and TUI should be cheap enough to be plausible on a small
machine. Pi Zero-class hardware illustrates that class; it is not a required
board or a release gate, and 512 MiB without swap is one measurement condition,
not a support promise. The website has its own budget.

- Report Control's own overhead separately from model execution, container
  backends and requested workload resources.
- Prefer event-driven idle behaviour, bounded projections and journal-backed
  history over polling, whole-state copies or repeated reconstruction.
- Avoid extra timers, duplicated state, repeated history hydration and new core
  dependencies. Keep web assets and optional adapters separable.
- Durable saves and synchronous filesystem work are deliberate correctness
  boundaries. Changing them needs crash and recovery evidence, not only timings.

## Measuring

Profiling fixtures are ignored tests. They use synthetic state, make no model
calls and need no credentials. Run them in release mode:

```sh
cargo test --release --locked --bin control <fixture path> -- --ignored --exact --nocapture
```

- Never point a fixture at the running node. Use fresh scratch
  `CONTROL_STATE_DIR` and `CONTROL_CONFIG_DIR` directories.
- For before/after work, copy the identical fixture into the baseline, keep the
  two builds in separate target directories and alternate their runs. Report
  medians of several runs and their ranges; overlapping ranges mean no change.
- Treat timings as evidence, not assertions. Correctness stays in normal tests.
- Linux flamegraphs: build with `CARGO_PROFILE_RELEASE_DEBUG=1`, run the test
  binary under `perf record -e cpu-clock:u -F 499 --call-graph dwarf,8192` with
  the same filter, then pipe `perf script` through `inferno-collapse-perf` and
  `inferno-flamegraph`. Only an isolated profiling container gets profiling
  privileges. On macOS, `sample` plus Inferno gives the same view.

Fixtures by subsystem:

| Subsystem | Fixture |
| --- | --- |
| Prompt and tool-schema size | `turn_context::tests::policy_size`, `worker_tools::deferred::tests::a_member_turn_needs_few_lookups` |
| Context construction | `conversation::tests::continuity_profile`, `conversation::tests::large_observation_profile`, `turn_context::tests::active_context_profile`, `turn_context::tests::trace_persistence_profile` |
| State saves | `storage::tests::profiling::profile_swarm_hour`, `storage::tests::profile_durable_save_stages`, `storage::tests::profiling::profile_checkpoint_load` |
| Bot reconciliation | `bots::tests::reconcile_profile`, `bots::tests::recovery`, `bots::tests::requests_profile` |
| Rooms and messages | `rooms::notifications::tests::profile_room_publication`, `rooms::notifications::tests::profile_idle_and_waiting_room_reconciliation`, `agent_message::profile::routing_profile` |
| Scheduling and cancellation | `scheduler::qa::scheduling_profile`, `chat::tests::profile_continuation_admission`, `lifecycle::tests::reconcile_profile`, `worker::tests::cancel_profile` |
| Services and watches | `services::commands::tests::dispatch_profile`, `services::commands::tests::live_latency`, `watches::tests::profile` |
| Web and HTTP | `performance::hot_paths`, `performance::ownership_snapshots`, `performance::history_reads`, `performance::chat_lists`, `performance::bots_page`, `performance::history_page`, `session_http::tests::fifteen_turn_conversations_workload` |
| Terminal (`--lib`) | `terminal::profiling` |
| Tools | `graph_search::tests::profile`, `compaction::tests::selection_profile`, `scores::tests::due_profile` |

## Rust benchmark tool

`control.rust.bench` (MCP) and `control_rust_bench` (managed workers) time a
standalone Rust expression. Use it to compare candidate implementations before
changing application code.

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

- Setup runs once. The tool warms up for `min(iterations, 100)` iterations,
  then times each sample batch with `Instant`. Results give raw batch
  nanoseconds, min/median/max per iteration and the image digest. With an even
  sample count the median is the upper one.
- Compilation and container startup are excluded. Black-box inputs and return
  the result to resist constant folding. Mutable setup state is shared across
  iterations; reset it inside `code` if that is part of the workload.
- Optimized Rust 2024, standard library only, no host files or network. Code
  plus setup is at most 14,000 bytes; 1–1,000,000 iterations; 3–31 samples.
- It runs in the playground's offline container: read-only root, 768 MiB, one
  CPU, 64 PIDs, 96 MiB `/tmp`. Compilation has 12 s and the program 3 s. Results
  are untrusted evidence.
- Old worker images fail compilation rather than report unoptimized timings;
  rebuild the image with setup after updating.

For a whole Cargo project, run its own benchmarks with the declared build
environment and `control_workspace_start`.

## Prompt and tool-schema size

Every wake starts a fresh provider session and resends its whole input, so
prompt bytes are the largest per-turn cost Control controls.

**Policy on demand.** Prompts carry a short core plus one index line per
on-demand lens; role-scoped lenses are omitted for other roles (see
[LENSES.md](LENSES.md)). `policy_size` builds a team shaped like a real swarm
(lead with `swarm` and `advisor`, a persistent member and the advisor, each
woken by a room post after a dozen callbacks). It fails if the policy section
exceeds 3,072 bytes for a member, 3,328 for a lead or 5,632 for the advisor
(the advisor keeps its procedure inline). Moving guidance out of the prompt cut
a member's wake text from about 26.5 KB to about 12 KB.

**Deferred tool loading.** `tools/list` holds each role's core tools; the prompt
indexes the rest, loaded with `control_tool_search`. `policy_size` fails if a
role lists 10,000 bytes or more of schemas, keeps more than a third of its
callable schema bytes, or has an index over 2,048 bytes. Current figures (Codex
backend, UTF-8 bytes):

| Role | Core schemas | All callable schemas | Index |
| --- | ---: | ---: | ---: |
| Lead | 9,604 | 62,348 | 1,718 |
| Member | 8,918 | 62,348 | 1,815 |
| Advisor | 3,664 | 13,933 | 680 |

Most of the saving comes from tight descriptions and compact schemas for the
core tools; spawn, bot and service tools stay core. In a scripted member turn
of 16 typical calls, 14 are core and the other two need one
`control_tool_search` by name (`a_member_turn_needs_few_lookups`). A local turn
loads an indexed tool on first use without a model step.

**Local planning reserve.** `OVERHEAD_TOKENS` in `src/local_agent.rs` is 7,168:
4,096 for core schemas, 2,048 for schemas loaded during the turn and 1,024 for
the first exchange. The local loop estimates 3 bytes per token, so the reserve
must cover that estimate even where a real tokenizer counts fewer
(`local_agent::tests::worker_tool_schemas_fit_the_declared_overhead_reserve`).

**Cache-stable prefix.** Turn input is ordered for provider prefix caches:
fixed framing and policy first, then history, with working and recovery notes,
per-run wake notes and the declared observation after it. Consecutive wakes
of the same member share all but about 300 bytes, so a local llama.cpp backend
reprocesses about 80 tokens per wake instead of the whole input. The prefix
breaks when the 20,000-character history window starts to slide or compaction
summaries change.

**Compact tool results.** Successful worker tool results pass through
`worker_tools::compact`: null, empty and default-false fields are dropped, and
top-level fields equal to the request's arguments are not echoed. Errors and
schema or guidance results are unchanged. Service command results use a
12-character `execution_id` handle (the full ID is also accepted; an ambiguous
handle is refused) and keep the head and tail of long output around one
omission marker. On a recorded swarm this cut visible tool output by about a
third, roughly 2,000 tokens per member turn.

Reproduce:

```sh
cargo test --locked --bin control policy_size -- --nocapture
CONTROL_PRINT_POLICY=1 cargo test --locked --bin control policy_size -- --nocapture  # print a member's wake text
CONTROL_PROMPT_DUMP=/scratch/prompts cargo test --locked --bin control dump_prompt_sections -- --ignored --nocapture
cargo test --locked --bin control deferred -- --nocapture
```

`dump_prompt_sections` writes each section to files for an external tokenizer,
such as `llama-tokenize` from the Nix-pinned llama.cpp.

## Context construction

`conversation::tests::continuity_profile` builds a managed turn context for 15
and 150 retained turns, including hydration of the previous attempt's archived
observations. Hydration is a deliberate correctness cost: later turns see
execution evidence and omitted user instructions. Excerpt limits bound prompt
output, but the reader still decodes the whole previous record, and
construction still indexes retained history, so bounded input is not constant
construction time. `large_observation_profile` measures 64 archived
observations of 4,000 characters (about 0.36 ms per prompt on Apple Silicon).

```sh
cargo test --release --locked --bin control conversation::tests::continuity_profile -- --ignored --exact --nocapture
cargo test --release --locked --bin control conversation::tests::large_observation_profile -- --ignored --exact --nocapture
CONTROL_PROFILE_PROJECTION=managed cargo test --release --locked --bin control \
  turn_context::tests::active_context_profile -- --ignored --exact --nocapture
cargo test --release --locked --bin control turn_context::tests::trace_persistence_profile -- --ignored --exact --nocapture
```

## State saves

Saves append one transaction line of changed paths to `state.log` and compact
into the `state.json` snapshot only when the log outgrows it (see
[HISTORY.md](HISTORY.md)). Before this, every save rewrote and synced the whole
state, which on an 18 MB node meant 8–34 GB written per hour of swarm work.

- `Run` is `history::Cached<Record>`: a save serializes a run once and reuses
  its bytes until the run is next borrowed mutably. Debug builds and tests check
  every save against a fresh serialization.
- Finished runs move into archive blocks; the hot state keeps the newest 512
  (`storage::archive::RECENT`) plus older resumable or still-referenced runs.
- Array diffs anchor on byte-identical elements, so removing runs from the
  middle of `runtime.runs` is a small splice.
- A clean start adopts the snapshot by indexing its bytes
  (`journal::snapshot::base`) instead of re-encoding the state.
- A torn final log line is dropped on load; earlier saves are intact.

`profile_swarm_hour` replays a simulated swarm hour of 2,000 saves (room posts,
agent messages, watch deliveries, tool receipts, new and finished turns) on a
copy of a real state file. On an 18.1 MB state (6,881 runs, 128 chats), APFS,
`F_FULLFSYNC`:

| 2,000 saves | Whole-file rewrite | Current |
| --- | --- | --- |
| Bytes written | 37.4 GB (18.7 MB/save) | about 1.2 KB/save (log line plus new and merged blocks) |
| Save p50 / p99 | 30.9 / 38.0 ms | 9.8 / 12.9 ms |
| Replay of a 2,001-line log | — | 72 ms |
| Clean start | 41.6 ms | 60 ms (snapshot plus archive blocks) |

About 60% of a save is now the sync. Not measured: APFS metadata writes and
concurrent saves (each save still syncs under the state mutex; there is no
group commit).

```sh
CONTROL_PROFILE_STATE=/scratch/state.json CONTROL_PROFILE_DIR=/scratch/bench \
  cargo test --release --locked --bin control \
  storage::tests::profiling::profile_swarm_hour -- --ignored --exact --nocapture
```

`CONTROL_PROFILE_STATE` must be a copy, never the live file. Optional
`CONTROL_PROFILE_SAVES` sets the save count and `CONTROL_PROFILE_MODE=before|after`
runs one path. `profile_durable_save_stages` times serialization,
overwrite plus fsync, atomic replacement and the complete save separately for
1/100/1,000 finished runs; tmpfs runs remove storage latency but do not test
durability.

## Bot reconciliation

`bots::tests::reconcile_profile` measures one reconcile pass on a task tree
with N helper chats (16 turns each, one in twenty failed), 128 live claims and
one persistent room bot. `idle` passes find nothing to do; `offered` passes are
forced through the full path. Reconciliation is event-triggered, not polled.
Release, Apple Silicon, medians:

| Case | No advisor | One advisor | Advisor, a quarter of claims blocked |
| --- | ---: | ---: | ---: |
| idle, 64 chats / 1,153 runs | 27.7 µs | 45.1 µs | 46.1 µs |
| offered, 64 chats / 1,153 runs | 35.5 µs | 52.6 µs | 55.6 µs |
| idle, 512 chats / 9,217 runs | 206.0 µs | 347.4 µs | 351.5 µs |
| offered, 512 chats / 9,217 runs | 214.1 µs | 355.5 µs | 358.7 µs |

Advisor lifecycle facts are computed only for trees whose root offers the
`advisor` lens; a defined advisor with the lens unticked costs the same as none.
An advisor adds one read-only pass over chats and runs (about 14 ns per run),
using a per-pass word-at-a-time hasher for chat IDs. Blocker reasons are
fingerprinted, so they are re-tokenised only when a blocked note or a failing
member's latest result changes. Terminal statuses are not final (operator retry
re-queues failed runs), so the pass does not use an incremental watermark.

```sh
cargo test --release --locked --bin control bots::tests::reconcile_profile -- --ignored --nocapture
```

Set `CONTROL_PROFILE_ADVISOR=1` for the advisor cases, `blocked` for the
blocked-claims case, `off` for a defined advisor with its lens unticked and
`antibody` for one [antibody](LENSES.md#antibodies) observing the node, with 8
services of 16 retained member commands from recent turns.

**Pathology facts and antibodies (2026-11-17).** Alternating base (`f4f8a23`) and
head runs, three each, medians in µs; the laptop was under heavy concurrent
load, so read these as ranges:

| Case | Base, none | Head, none | Base, advisor | Head, advisor | Head, antibody |
| --- | ---: | ---: | ---: | ---: | ---: |
| idle, 64 chats | 29.5 | 28.9 | 49.0 | 50.2 | 61.7 |
| offered, 64 chats | 37.3 | 37.8 | 57.1 | 58.8 | 68.4 |
| idle, 512 chats | 223.4 | 225.7 | 386.9 | 388.8 | 416.9 |
| offered, 512 chats | 228.2 | 236.5 | 398.9 | 400.9 | 422.3 |

Nodes without an observer are unchanged. Pathology inputs add a probe per run
only until every retained command's run is resolved (newest first), command
classification only when a team's retained set changes, and a token re-sum only
when a team's settled-turn count changes; summing tokens on every pass cost
about 20 µs at 9,217 runs and was dropped.

**Bounded recovery.** Checkpoint recovery is bounded per streak (see
[TURN-CONTINUITY.md](TURN-CONTINUITY.md)). The `bots::tests::recovery` replay
drives a bot whose every handoff is rejected: over 3 simulated hours it takes 8
turns and 2 `continue` wakes; without the bound it took 181 turns, limited only
by the 60-turns-per-hour budget.

**Handoff grace.** After an accepted handoff the provider has up to 20 s
(`HANDOFF_GRACE`) to end its turn, so Codex reports the turn's usage. Codex is
stopped at once if it starts shell or file work after the handoff.

**Peer messages.** Each reconcile pass also scans agent-message receipts for
overdue digest flags; retired receipts drop their flag. Non-urgent peer
messages wait for the recipient's next wake or the digest bound instead of
waking it. Observation refreshes run on their own thread, outside the state
lock.

## Rooms and directed messages

`profile_room_publication` admits and publishes one room notification through
reconciliation with a no-op checkpoint. `profile_idle_and_waiting_room_reconciliation`
measures an armed, waiting subscription (`CONTROL_ROOM_PROFILE_ROUNDS`, default
10); the unarmed path is at the measurement floor. Both still scan retained
runs linearly: about 1 ms and 0.76 ms at 20,001 retained runs on one pinned
Linux ARM64 CPU.

`agent_message::profile::routing_profile` measures the downward message path
with in-memory construction, result encoding and dispatcher notification, but
no fsync, peer wake or provider work: about 0.9 µs at 16 chats and 3.5 µs at
1,024 on one pinned Linux ARM64 CPU.

```sh
CONTROL_PROFILE_ROUNDS=20000 cargo test --release --locked --bin control \
  agent_message::profile::routing_profile -- --ignored --exact --nocapture
```

## Scheduling and cancellation

- `scheduler::qa::scheduling_profile` compares `control-core` scheduling with
  the previous scheduler over long ledgers (5,032 runs). Ordinary cases are
  within noise (about 3–9 µs); a paused node returns before reading history.
  The application tests also check both schedulers agree over 2,000
  deterministic mixed ledger states.
- `chat::tests::profile_continuation_admission` measures queueing a
  continuation, including authorization, capacity, queue-order scan and
  dispatcher notification: about 0.24 µs at 16 retained runs and 100 µs at
  20,001, where the ledger scan dominates.
- Container ownership reconciliation runs every 2 s in the dispatcher and stops
  inactive owners' containers. Attempts are indexed by generation once per pass;
  `lifecycle::tests::reconcile_profile` with 48 containers takes 0.09 ms at 500
  attempts and 0.73 ms at 12,000 (it was linear per container before indexing,
  483 ms at 12,000).
- `worker::tests::cancel_profile` runs the real dispatcher, workers, durable
  checkpoints and commit path on 127 chats and 5,773 runs with seven running
  turns. Cancelling all seven took 417 ms and 11 checkpoints.

```sh
cargo test --release --locked --bin control scheduler::qa::scheduling_profile -- --ignored --exact --nocapture
```

## Services and watches

**Command dispatch.** The service supervisor dispatches when a command is
admitted or a request status changes (one shared `Changes` signal), and also on
a 2 s tick for engine reconciliation. Exec and copy wait inline up to
`wait_seconds` (default 15, max 30) without the state lock and return the final
result if the command finishes in time.

`services::commands::tests::live_latency` creates a service from the pinned
worker image and runs 40 `echo ok` commands under each policy (debug build,
Docker Desktop, Apple Silicon):

| Policy | submit→start p50 / p95 | submit→result p50 / p95 |
| --- | ---: | ---: |
| 2 s tick, then a status poll | 1,018 / 1,910 ms | 1,102 / 1,996 ms |
| Wake on admit, inline wait | 20 / 25 ms | 107 / 189 ms |

The remaining ~85 ms is `docker inspect` plus `docker exec`. The tick-only row
is a lower bound: a real poll adds a model round trip.

`dispatch_profile` measures one pass with nothing to launch. A pass reads each
retained command record once (about 6 ns each): about 0.4 µs for 8 idle
services and 15 µs for 128 services with 64 running commands each. Records per
service are bounded by the command ceiling plus 16 finished ones, and a running
record holds no command text, so it stays under about 18 KB
(`persisted_command_records_stay_bounded`).

```sh
cargo test --release --locked --bin control services::commands::tests::dispatch_profile -- --ignored --nocapture
CONTROL_QA_SERVICE_IMAGE=<image id> CONTROL_STATE_DIR=<scratch dir> \
  cargo test --locked --bin control live_exec_latency -- --ignored --nocapture
```

`CONTROL_PROFILE_ROUNDS` sets passes per batch; `CONTROL_QA_ROUNDS` sets the
live fixture's rounds.

**Watch salience filters.** `Watch::observe` runs under the state lock for every
frame a follower reads. `watches::tests::profile::notification_observe_profile`
feeds 200,000 JSON notifications through one watch (release, Apple M3 Max):

| Filter | ns/event |
| --- | ---: |
| none | 154 |
| one `threshold` | 400 |
| `key`, two `change` rules and a `rate_limit` | 532 |
| `change` and `debounce` | 575 |
| `rate`, `zscore` (32 samples) and `hysteresis` | 652 |

An unfiltered watch reads no clock and does no parsing. The follower's spool
append costs about 26 µs per frame, so filters add 2–3% to a frame's host path,
and every notification they hold back avoids a model wake. Not measured: Linux,
state-lock contention and end-to-end wake latency.

```sh
cargo test --release --locked --bin control watches::tests::profile -- --ignored --nocapture
```

**Watch collector.** The service runner's `watches::tests::collector_profile`
collects framed output from eight processes of 20,000 lines each. Enabling
notifications adds a descriptor and bounded buffer to the existing collector
thread, not a thread per channel. Run it only in an isolated Linux container
with a private home and writable `/workspace`, without credentials:

```sh
rustc --edition=2024 --test -O -C debuginfo=1 tools/service-runner/main.rs -o /tmp/runner-tests
/tmp/runner-tests watches::tests::collector_profile --ignored --exact --nocapture
```

`CONTROL_WATCH_PROFILE_BATCHES=128` gives longer runs for sampling.

## Web, HTTP and chat delivery

**Transcript rendering.** The web renderer borrows typed messages, runs and
activity instead of building intermediate JSON, and uses request-local run
indexes. Ownership snapshots keep identity fields without copying command and
log buffers. macOS ARM release medians:

| Workload | Median |
| --- | ---: |
| 10 messages | 130 µs |
| 100 messages, 1,000 older unrelated runs | 0.74 ms |
| 500 messages, 5,000 older unrelated runs | 3.12 ms |
| 500 attempt ownership snapshots | 310 µs |

Normal tests compare rendered HTML against SHA-256 fingerprints.

**Chat lists and callsigns.** Callsigns are derived on read
(`callsigns::all`: one pass in creation order with a ring of the last 48).
`performance::chat_lists` renders the chat list, the web conversation tree and
one chat read for 10, 100 and 1,000 chats. At 1,000 chats: list about 2.0 ms,
tree about 0.14 ms, one chat read about 14 µs.

**Bots page and graveyard.** The graveyard (stopped bots and finished spawned
agents) is projected in one pass over runs and chats, sorted by stop time, and
only one page of 50 tombstones is formatted; later pages come from
`GET /bots/graveyard?older=ID[&q=…]` as the WASM client scrolls, and search
filters the same pass. The fennec skull is one inline SVG `<symbol>` per page;
each stone is a `<use>` of it, and the staggered layout is CSS on list
position, so appended pages add no path data or layout script. The process
tree indexes latest run statuses, chat ids and service owners once per render;
it used to scan runs and chats per chat, which made the page quadratic.
`performance::bots_page` holds 10, 1,000 or 10,000 dead bots (one turn each)
and one live agent. Release, Apple Silicon, `CONTROL_TIMING_ROUNDS=51`, three
alternating runs against the previous revision with the same fixture, medians:

| Workload | 10 | 1,000 | 10,000 dead bots |
| --- | ---: | ---: | ---: |
| Bots page, before (no graveyard) | 9.7 µs | 0.77 ms | 90.7 ms |
| Bots page, graveyard list | 29.1 µs | 0.40 ms | 4.37 ms |
| Bots page, tombstone grid | 27.4 µs | 0.39 ms | 4.08 ms |
| Next graveyard page, list → grid | 1.6 → 1.6 µs | 0.23 → 0.22 ms | 2.55 → 2.17 ms |
| Graveyard search, list → grid | 4.3 → 4.1 µs | 0.28 → 0.27 ms | 2.80 → 2.34 ms |

The page stays under 38 KB however many bots have died (39 KB as a list); a
50-stone scroll page is about 31 KB.

**Bots / History.** One view lists DMs, newest activity first, 50 per page
(`history_view`). Sorting needs one pass over chats; the listed rows' turn
count, failures, active turns, tokens and latest model need one pass over runs
that only adds up turns of those rows (a word-at-a-time hash keys the run's DM).
A row's turns arrive on demand from `GET /bots/history/turns?dm=ID`, 50 at a
time; a status, kind or model filter adds one pass that keeps at most five
matching turns per DM, and "Show turns" keeps five in the summary pass. Each
page lists the models of the newest 2,000 turns only. `performance::history_page`
requests each target through the HTTP handler with 10, 1,000 or 10,000 DMs
sharing 100,000 turns (one in twenty failed). Release, Apple Silicon,
`CONTROL_TIMING_ROUNDS=21`, three alternating runs against the previous
revision with the same fixture, median of the run medians (bytes in brackets):

| Workload | 10 | 1,000 | 10,000 DMs |
| --- | ---: | ---: | ---: |
| Before: Conversations tab (10 DMs, latest status) | 14 µs | 0.10 ms | 1.11 ms |
| Before: Runs tab (10 runs) | 22 µs | 21 µs | 20 µs |
| History page | 4.2 ms [11 KB] | 4.4 ms [34 KB] | 5.2 ms [34 KB] |
| Failed turns only | 2.5 ms [9 KB] | 4.4 ms [224 KB] | 5.3 ms [224 KB] |
| Show turns | 5.0 ms [42 KB] | 5.6 ms [192 KB] | 7.3 ms [193 KB] |
| Name search | 0.96 ms | 1.6 ms | 2.9 ms |
| One DM's turns (fragment) | 0.12 ms [35 KB] | 1.6 ms [38 KB] | 3.3 ms [8 KB] |

The old tabs were cheaper because they summarised nothing: Conversations
showed ten names with the latest status and stopped scanning runs once it had
found them, and Runs showed ten runs. The one view adds up every listed DM's
turns, so its cost follows the number of turns (about 40–50 ns each), not the
number of DMs. Individual runs varied by up to 2× on a busy machine.

```sh
CONTROL_TIMING_ROUNDS=21 cargo test --release --locked --bin control performance::history_page -- --ignored --exact --nocapture
CONTROL_TIMING_ROUNDS=51 cargo test --release --locked --bin control performance::bots_page -- --ignored --exact --nocapture
cargo test --locked --release --bin control performance::hot_paths -- --ignored --exact --nocapture
cargo test --locked --release --bin control performance::ownership_snapshots -- --ignored --exact --nocapture
CONTROL_TIMING_ROUNDS=300 cargo test --release --locked --bin control performance::chat_lists -- --ignored --exact --nocapture
CONTROL_STATE_DIR=/scratch/http-bench CONTROL_PROFILE_CONVERSATIONS=40 cargo test --release --locked --bin control \
  session_http::tests::fifteen_turn_conversations_workload -- --ignored --exact --nocapture
```

**Request panel.** `bots::tests::requests_profile` renders a chat with zero or
32 pending bot approvals as a transcript plus the separate request document
(about 1 µs and 22 µs).

**Live updates.** Web and TUI clients receive coalesced, authenticated,
payload-free change hints over a stream instead of polling the transcript. Idle
streams wait on the effect generation signal with 15 s heartbeats and never
wake agents; reconnects force a fresh read. In one comparison with direct
Codex, this brought Control's time to first message text within about 0.4 s of
running Codex directly, from about 2.5 s with polling. Five turns cannot
establish population latency.

**HTTP admission.** At most 32 HTTP handlers run at once
(`http_admission::MAX_HANDLERS`), alongside at most 16 live streams. With 16
streams and 128 slow partial request bodies, the server held 39 threads and
38 MiB RSS instead of 151 threads and 82 MiB, and shut down in 0.06 s instead
of 10 s. Ordinary health requests were unchanged (about 28 µs on loopback).
This bounds admission; it is not a whole-process memory budget. See
[HTTP-ADMISSION.md](HTTP-ADMISSION.md).

## Terminal

The TUI caches the formatted transcript and UTF-8 row ranges until its data or
view changes. Resize rebuilds the row index; scrolling borrows only visible
rows. A warm frame costs about 59 µs at 10, 100 or 500 messages and about
60–71 µs at 1,000–10,000 messages; building the index once takes about 2 ms
for 1,000 messages and 24 ms for 10,000. Network refresh is not part of these
offline fixtures.

```sh
cargo test --release --locked --lib terminal::profiling -- --ignored --nocapture --test-threads=1
```

The server-only build omits terminal code and its dependencies
(`cargo build --release --locked --no-default-features --bin control`, or
`nix build .#control-server`); its stripped Linux binary is about 4% smaller.

## Tools

**Path search.** `control_search_path` limits are 20,000 nodes, 100,000 edges,
1,000,000 cells, 2,000,000 expansions and 2 s. `graph_search::tests::profile`
(release, Apple Silicon, median of five calls, including JSON parsing):

| Case | A* | Dijkstra |
| --- | ---: | ---: |
| graph, 20k nodes / 100k edges | 27.0 ms | 25.9 ms |
| grid 1000×1000, 4-connected, 10% blocked | 9.3 ms | 78.8 ms |
| grid 1000×1000, 8-connected, 10% blocked | 40.0 ms | 79.6 ms |
| grid 100³ open, 26-connected | 2.0 ms | — |

Graph calls are dominated by parsing the input (17.3 ms for the largest graph).
`control_room_plan` walks at most one room's claims linearly.

```sh
cargo test --release --locked --bin control graph_search::tests::profile -- --ignored --nocapture
```

**Compaction selection.** `compaction::tests::selection_profile` measures
cached summary selection with mocked model replies. It revalidates source
hashes before reusing a view, so cost is linear in the retained source (about
11 µs at 32 entries, 320 µs at 1,024); the selected summary cut is logarithmic.

**Score probes.** The score supervisor wakes every 5 s and, only when probes
exist, makes one pass over chats, runs and claims under the state lock to count
team milestones; probe commands run without the lock. Series are capped at 240
samples per probe and 32 probes per node (`scores::MAX_SAMPLES`,
`scores::MAX_PROBES`). `scores::tests::due_profile` measures the pass.

**Local models.** `control_model_generate` runs only when called. Plain-HTTP
endpoints use a small std client that checks cancellation every 100 ms; bodies
are capped at 128 KiB. Inference figures are in [LOCAL-MODELS.md](LOCAL-MODELS.md).
