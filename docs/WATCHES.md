# Watch notifications and diagnostic output

## Routines

Agent-facing tools call a watch's program a *routine*: a program an agent
writes and starts that works toward a target on its own, handles its own steps,
retries and events, and escalates only milestones, failures and situations it
cannot handle. Routines are the swarm's reflexes and tactics; model turns are
its strategy and planning. Like a hive's drones keeping the nest in order, or
sentinel machinery that notices a ship long before the swarm is roused, they run
without attention so that the collective's attention is spent on decisions.

`control_watch_start` starts a routine; `control_watch_events` reads the outcomes
it reports; `control_service_exec` is for one-off setup, inspection and
installing or editing a routine. There is no separate routine tool: one schema
and one implementation keep the tool list short and its policy in one place.
When an attempt has issued five commands to the same service and no routine
(active watch) runs there, the fifth exec result carries a one-line `hint`
suggesting one. It appears once per attempt and service, changes nothing else,
and is computed from that service's retained command records.

Use separate channels for control events and stdout/stderr. A watch started with
`notifications=true` supplies an inherited descriptor, `CONTROL_NOTIFY_FD=3`.
The program writes explicit newline-delimited notification payloads there; normal
output remains diagnostic evidence. New notification watches require
`wake_policy=exit_only` (the default); log-driven policies remain available only
for legacy-style watches. Each payload is bounded to 4096 bytes. The
runner frames notifications separately, and no log prefix or severity parser can
select or suppress them. Both descriptors use the existing reader thread.

```json
{
  "service_id": "svc-example",
  "name": "worker",
  "command": "exec /workspace/worker",
  "restart": "permanent",
  "notifications": true,
  "wake_policy": {"kind": "exit_only"}
}
```

For example, an adapter can write the following line to the descriptor after it
has recorded the result for that exact task:

```json
{"task_id":"build-54","outcome":"completed","evidence":"/workspace/results/build-54.json"}
```

This is an example payload, not a protocol the supervisor interprets. Downloads,
builds and other workloads can choose their own event schemas. Adapter results
are untrusted program claims; process exits are independently observed by the
host. Neither grants authority or establishes that an arbitrary user goal was met.
Writers sharing the descriptor must serialize their complete lines. Oversize
payloads are marked truncated and require evidence resynchronisation.
Adapters spawning another process must explicitly preserve the descriptor if
that child is the notification emitter.

Register and test the completion channel before dispatching work. Every task
whose result releases a dependency wait must emit an outcome for its exact ID,
including failure. Do not restrict completion delivery to a task-name prefix.
Before yielding, verify that the handoff waits on that watch topic. A watch of a
long-lived process does not make its internal task completions observable: process
exit only reports the lifetime of the process. Finite tasks can instead use a
transient watch whose process exits when the actual operation finishes. No
implicit task deadline or periodic model check-in is needed.

Notifications enter the existing watch topic, budgets, coalescing and publication
receipt path. They release waits on that topic without interrupting unrelated
dependency waits. An active model turn can consume them without starting an
additional turn. Logs and notifications retain their source sequence in the
existing bounded journal; this is not unlimited or exactly-once external effect
delivery. Source loss remains explicit and requires inspecting durable results.

For notification-enabled watches, `control_watch_events` returns notifications
and process exits by default. Diagnostic lines do not consume that view's
200-event page budget. Use `include_logs=true` and an explicit historical `after`
cursor when investigating logs. Reading either view does not acknowledge it.
Changing views with the same request identity is rejected for an already
delivered nonempty source range. Empty polls keep the existing position-based
receipt behavior.

The notification descriptor is opt-in for compatibility (service-runner image
label 4). Old runner images
reject `notifications=true` instead of silently starting without the channel;
start a service from rebuilt images to use it. Existing service processes and
watches are not upgraded or migrated in place.

## Salience filters (perception)

Routines filter what they observe and escalate only salient changes. A routine
can do that in its own code, but fast loops, scrapers and feeds tend to report
every observation. A notification watch can declare `salience` on
`control_watch_start`: a small JSON filter that decides which notifications wake
the agent. It is generic data evaluated by Control on the payload, with no
knowledge of what a field means. Filtered notifications are still spooled and
returned by `control_watch_events`; they only do not wake. `control_watch_status`
and `control_watch_events` report `salience.woke`, `salience.retained` (did not
wake on arrival) and `salience.holding` (debounced, may still wake).

A filter is `rules`, each an ordered list of stages. A notification wakes when
any rule passes. Within a rule, each stage sees only events that passed the
stages before it, so order matters: a gate placed before `hysteresis` hides the
values hysteresis needs to re-arm.

| Stage | Passes when |
| --- | --- |
| `threshold` `field`, `at_least`/`at_most` | the number is at or past a bound; a rule of its own is a severity passthrough |
| `change` `field`, `percent`/`absolute` | the value moved that much from the last *woken* value; with neither, any different value (dedup, also for strings and objects) |
| `rate` `field`, `seconds`, `at_least` | \|Δvalue\| per second over the window reaches `at_least` |
| `zscore` `field`, `samples`, `at_least` | the value is `at_least` standard deviations from the previous `samples` (5–64) values |
| `hysteresis` `field`, `trigger`, `clear`, `clear_seconds` | the value reaches `trigger` while armed; it then re-arms only after staying past `clear` for `clear_seconds`. `trigger < clear` watches a low side |
| `debounce` `seconds` | holds the latest event and wakes with it after `seconds` without another; stages after it run at release |
| `rate_limit` `seconds` | no wake for this key in the last `seconds` |

`field` and `key` are JSON pointers into the payload (`/price`, `/data/0/hp`).
`key` partitions all state, e.g. one baseline per symbol. `change` and
`rate_limit` compare with the last wake of that key from any rule; `rate`,
`zscore` and `hysteresis` update from every event that reaches them. A debounce
hold is dropped if a later event fails the stages before it (the change
reverted), and any wake supersedes the key's holds. A quiet routine's hold is
released by the next supervisor pass, within about two seconds of its due time.
Payloads that no rule can read (not JSON, or a missing or non-numeric field in
every rule) wake by default so completion and failure notices without those
fields still arrive; `"unmatched":"retain"` keeps them as events only. Process
exits and source gaps bypass the filter. A filter requires `notifications=true`,
is captured at creation and allows 1–4 rules of 1–6 stages.

State lives in the watch record and survives restart. It is bounded: at most 8
keys (least recently seen evicted, sparing holds), 64 window samples per key,
200-character previews and long values compared by digest.

Price monitoring — wake on a move over 2% at most hourly, but always on one over
5%, per symbol:

```json
{"key":"/symbol","rules":[
  [{"kind":"change","field":"/price","percent":5}],
  [{"kind":"change","field":"/price","percent":2},{"kind":"rate_limit","seconds":3600}]
]}
```

Scrape diff — wake when the page hash changes and has stayed changed for a minute:

```json
{"rules":[[{"kind":"change","field":"/hash"},{"kind":"debounce","seconds":60}]]}
```

Game threats — commit to a threat once, re-arm only after three clear seconds,
and pass severe events straight through:

```json
{"rules":[
  [{"kind":"hysteresis","field":"/threat","trigger":0.8,"clear":0.4,"clear_seconds":3}],
  [{"kind":"threshold","field":"/severity","at_least":9}]
]}
```

Run -128's reflex jitter came from re-deciding on every tick with a single
threshold and no commitment: a threat score oscillating around it flipped the
mode each time. Hysteresis is the commit. Run -121's record spam came from routine
records reaching the model as they were written; dedup, change and rate limits
let them stay evidence. The same rule applies inside a reflex loop: a filter
reduces wakes, it does not make an in-process controller commit.

## Legacy log-driven wake selection

`control_watch_start` runs a supervised command in an authorized persistent
service. Its output is spooled independently of whether it wakes an agent.
`wake_policy` selects line events before they reach the generic bot supervisor:

| Policy | Eligible events |
| --- | --- |
| `{"kind":"any_line"}` | Every line, process exit and detected source gap. Explicit opt-in; legacy saved watches retain this behavior. |
| `{"kind":"exit_only"}` | Default for new watches. Process exits and detected source gaps. All lines remain readable. |
| `{"kind":"prefixes","prefixes":["event:","error:"]}` | Lines starting with either literal prefix, plus process exits and detected source gaps. |

Prefixes are case-sensitive, anchored at the start, and interpreted as bytes,
not regular expressions. Supply 1–8 nonempty prefixes of at most 128 UTF-8 bytes
each without control characters. Include every event class the agent needs;
the host cannot infer a program's protocol or recognize a failure hidden in an
unselected line. Process exits bypass line selection even when the owner started
the process. A watch's creator does not identify the cause of each stream line.

Workload tools should emit selected lines when a dependency becomes ready, a
milestone changes, or an error needs attention. Keep unchanged health, status
query responses and routine telemetry outside those prefixes. Matching a common
prefix on every diagnostic line recreates any-line waking. Domain-specific
transition detection belongs in the workload container, not Control's supervisor.
Routine command completions already returned to the active worker, including
surveys and inspections, should remain diagnostics rather than produce another
coordination wake. Emit a callback when a waiting worker needs to resume for a
changed dependency, and retain the underlying evidence for deliberate reads.

Classification is workload policy: observe the previous condition and the new
condition, retain diagnostics, and emit an actionable transition only when the
condition changes. An unknown failure can require investigation on first sight;
once understood, repetitions of that same condition stay in diagnostics. Recovery
and a later recurrence are new transitions. A process exit or source gap remains
eligible independently of any classification. Do not classify all lines containing
`ERROR`, `Failed` or `Exception` as actionable: the playtest's repeated optional
service DNS warnings passed such a filter and created monitoring-only turns.
Likewise a crop age change can be retained while its readiness callback waits for
the actual harvest condition. These examples specify tool behavior, not host
knowledge of Minecraft or a universal list of ignorable failures.

Validate a workload's emitter with repeated known-warning observations, first
unknown failure, recovery and recurrence. Assert the expected transition events,
then replay those events through the generic watch policies. Keep this distinct
from end-to-end gameplay benchmarks; instructions alone do not prove the emitter
implements the policy correctly.

Example arguments for a service whose log uses those prefixes:

```json
{
  "service_id": "svc-example",
  "name": "events",
  "command": "tail -F /workspace/events.log",
  "wake_policy": {"kind": "prefixes", "prefixes": ["event:", "error:"]},
  "wake": true
}
```

The service must already be owned by or granted to the caller. `wake` arms a
non-bot chat once; `wake=false` does not disable an existing bot. Explicitly
arming through `control_watch_wake` respects each watch's policy. Existing bot
budgets, waits, coalescing and single-writer admission still apply. Policy is
captured at creation; use a new watch/request identity to replace a policy.
Retries of a start request return the existing watch's captured policy.

`control_watch_status` reports the policy, service generation and creator
run/attempt when known. Legacy watches have unknown creation provenance and
retain their any-line behavior. The pending line count and wake cursor refer to
selected lines; the source cursor tracks every observed frame. Quiet lines after
a selected event do not move that event's wake cursor or enter its prompt preview.

For legacy watches, `control_watch_events` reads unfiltered retained frames with original sequence
numbers and truncation flags. It returns `source_gap=true` when the requested
range has missing or truncated evidence, including a missing spool or tail.
Owned watches return a receipt in the existing attempt mailbox. Pass it to
`control_turn_handoff.processed` after processing the evidence; only successful
workspace/handoff publication advances the processed watermark. The default read
starts after the last delivered or committed position, while explicit `after`
can inspect history. Retries retain their delivered range. Skipped ranges and
source gaps require explicit resynchronisation before publication. A foreign
watch remains an observation without a receipt, even with a service grant.
Reading alone does not acknowledge processing. Source sequence jumps generate an eligible gap event;
missing local files are reported on read, not through a new background monitor.
Existing retention/rotation and per-line limits still apply. Inspection of a
service's current state may be needed after a gap.

This implements R2's generic watch transport seam. It does not parse game-specific
messages, add timers, or infer that an arbitrary goal
is complete. Existing watches need an explicit filtered replacement to reduce
line-driven wakes. Synthetic tests cover noisy lines, selected events, exits,
save failure/restart and missing evidence. Diamond-run improvement still requires
a controlled baseline comparison after deployment.

## Score probes

Claims and messages are weak progress measures: their count depends on how
work is split. A chat can instead declare how its workload scores itself with
`control_score_probe`: a quick, read-only command in one of its services that
prints a JSON object of up to 8 numbers on its last line, such as
`{"blocks_placed":75,"beds":2}` for a game, `{"items":120}` for a scraper or
`{"alerts":3}` for monitoring. Earlier output lines are ignored diagnostics.

```json
{"name": "score", "service_id": "svc-example", "command": "cat /workspace/score.json", "interval_seconds": 300}
```

Control runs the command once when it is declared (10 s bound) and rejects a
probe that does not produce a score. It then samples it every
`interval_seconds` (60 s to 1 day, default 300) and when the team reaches a
milestone: a turn completes or a claim is done anywhere in the chat's team, at
most every 30 s. Samples run in the service as its user, in `/workspace`, with
a 20 s bound; a command still running then is stopped inside the service.
A failed sample records its error and no value. Up to 4 probes per chat and 32
per node; redeclaring a name replaces the probe and `clear=true` removes it.
Probes of deleted chats are dropped; a probe whose service stopped or was
replaced reports that until it is declared again.

Each probe keeps at most 240 samples (`time`, `trigger`, `values`). When full,
the older half is thinned to every second sample, so a series covers its whole
run at falling resolution while recent samples stay complete. At the caps a
probe is about 60 KB of state.

The operator reads a run's series with `control.run.scores` (any chat id in the
team; returns every probe in the root chat and its descendants and the team's
milestone count). The chat view shows each probe's latest values in one line.
Control does not interpret names or values: they are the workload's own
claims, kept for operators and offline scorecards that relate them to model
turns, tokens and time. Probes are the generic capability; nothing in Control
knows any particular workload.

## Fresh observations

A chat can declare one observation source with `control_observe_set`: the
latest output of a short command in one of its running services, or a watch's
newest notification (else its latest lines). Control places that output,
capped, at the end of every turn's input (after the cache-stable prefix; see
[prefix caching](LOCAL-MODELS.md#prefix-caching)), and of `control_service_exec` results
when declared with `exec_results=true`. The agent sees current state without a
status tool call; the evidence motivating it was a run where members called
`control_service_exec_status` 969 times and `control_turn_context` 501 times.

```json
{"service_id":"svc-example","command":"status --brief","refresh_seconds":10,"timeout_seconds":5,"max_bytes":2048}
```

- One source per chat; a new declaration replaces it, `clear=true` removes it
  and a call without arguments shows it with its current value. The service
  must be running and usable by the chat, and is rechecked at every refresh; a
  watch must belong to the chat or to a service it can use.
- A command reruns at turn start when its last refresh is older than
  `refresh_seconds` (default 10, 0 = every turn). The turn waits at most 2 s
  for it (an exec result 0.5 s); otherwise, or when it times out
  (`timeout_seconds`, default 5, max 30) or fails, the block shows the last
  value with its age and the reason. The refresh keeps running in the
  background, at most one per chat, and a timed-out command is stopped inside
  the service like an exec. It uses no exec slot or execution record.
- A watch source runs nothing: it reuses the watch's latest notification or
  lines and notes when the watch has ended.
- `max_bytes` defaults to and is capped by `observation_max_bytes`
  (execution.json, default 2048, 256–16,384), cut on a character boundary.
- Values live in memory only. They never enter the run objective, chat
  transcript, journal or checkpoint; the declaration persists as chat
  configuration. After a restart the first turn refreshes it.
- Output is untrusted program data, never instructions or authority. Keep the
  command read-only; it runs with the same authority as an exec in that service.

The source is generic: a game status query, a scraper's progress file or a
monitor's summary all fit. Domain parsing stays in the command or the routine.
