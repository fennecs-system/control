# Watches, routines and observations

## Routines

A *routine* is a program an agent writes and starts in one of its persistent
services. It works toward a target on its own, handles its own steps, retries and
events, and reports only milestones, failures and situations it cannot handle.
Routines are the swarm's reflexes and tactics; model turns are its strategy.

| Tool | Use |
| --- | --- |
| `control_watch_start` | Start a routine (a supervised command in an authorized service) |
| `control_watch_events` | Read the events it reported |
| `control_watch_status` | Inspect its policy, cursors and provenance |
| `control_service_exec` | One-off setup, inspection, and installing or editing a routine |

There is no separate routine tool: one schema and implementation keep the tool
list short and the policy in one place. A chat can run up to 8 active watches.
`restart=permanent` (default) restarts the program with backoff after any exit;
`restart=transient` runs finite work once. No task deadline applies. Settings are
captured at creation; start a new watch to change them.

When one attempt has issued five commands to the same service and no routine runs
there, the fifth exec result carries a one-line `hint` suggesting one. It appears
once per attempt and service and changes nothing else
(`src/services/commands/routine.rs`).

## Notification channel

Keep control events separate from stdout/stderr. A watch started with
`notifications=true` gets an inherited descriptor, `CONTROL_NOTIFY_FD=3`. The
program writes newline-delimited notifications there; ordinary output stays
diagnostic evidence. Notification watches must use `wake_policy=exit_only` (the
default). Each notification is at most 4096 bytes; longer ones are marked
truncated and need resynchronisation from durable evidence. No log prefix or
severity parser can select or suppress notifications.

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

An adapter might write this after recording a task's result:

```json
{"task_id":"build-54","outcome":"completed","evidence":"/workspace/results/build-54.json"}
```

Control does not interpret the payload; each workload chooses its own schema.
Notifications are untrusted program claims. Process exits are observed by the
host. Neither grants authority or proves that a goal was met. Writers sharing the
descriptor must write whole lines. A process that spawns the real emitter must
pass the descriptor on.

Register and test the channel before dispatching work. Every task whose result
releases a dependency wait must emit an outcome for its exact ID, including
failures. Before yielding, check that the handoff waits on the watch's topic. A
long-lived process's exit says nothing about its internal tasks; for finite work,
a transient watch whose process exits when the work finishes also gives a
host-observed exit.

Notifications use the existing watch topic, budgets, coalescing and receipts.
They release waits on that topic without interrupting unrelated waits, and an
active turn can consume them without starting another. Logs and notifications
keep their source sequence in a bounded journal; this is not exactly-once
delivery. Source loss is reported and requires inspecting durable results.

`control_watch_events` on a notification watch returns notifications and process
exits by default; logs do not use the 200-event page. Pass `include_logs=true`
and an explicit `after` cursor to read logs. Reading does not acknowledge. A
retried request may not switch views over a range already delivered.

The channel needs a service image whose runner supports `notify-v1`
(`dev.control.service-runner` label 4). Older runners reject
`notifications=true`; start the service from rebuilt images. Running services and
watches are not upgraded in place.

## Salience filters

Fast loops, scrapers and feeds tend to report every observation. A notification
watch can declare `salience` on `control_watch_start`: a small JSON filter,
evaluated by Control on the payload, that decides which notifications wake the
agent. Control knows nothing about what a field means. Filtered notifications are
still retained and returned by `control_watch_events`; they just do not wake.
`control_watch_status` and `control_watch_events` report `salience.woke`,
`salience.retained` (did not wake on arrival) and `salience.holding` (debounced,
may still wake).

A filter has `rules`, each an ordered list of stages. A notification wakes when
any rule passes. Each stage sees only events that passed the stages before it, so
order matters: a gate before `hysteresis` hides the values it needs to re-arm.

| Stage | Passes when |
| --- | --- |
| `threshold` `field`, `at_least`/`at_most` | the number is at or past a bound; alone in a rule it is a severity passthrough |
| `one_of` `field`, `values` | the value, as text, is one of up to 16 listed strings; with `"unmatched":"retain"` a routine that reports many outcomes wakes only for the named ones |
| `change` `field`, `percent`/`absolute` | the value moved that much from the last *woken* value; with neither, any different value (dedup, also for strings and objects) |
| `rate` `field`, `seconds`, `at_least` | \|Δvalue\| per second over the window reaches `at_least` |
| `zscore` `field`, `samples`, `at_least` | the value is `at_least` standard deviations from the previous `samples` (5–64) values |
| `hysteresis` `field`, `trigger`, `clear`, `clear_seconds` | the value reaches `trigger` while armed; it re-arms only after staying past `clear` for `clear_seconds`. `trigger < clear` watches a low side |
| `debounce` `seconds` | holds the latest event and wakes with it after `seconds` without another; later stages run at release |
| `rate_limit` `seconds` | no wake for this key in the last `seconds` |

`field` and `key` are JSON pointers into the payload (`/price`, `/data/0/hp`).
`key` partitions all state, for example one baseline per symbol. `change` and
`rate_limit` compare with the key's last wake from any rule; `rate`, `zscore` and
`hysteresis` update from every event that reaches them. A debounce hold is dropped
if a later event fails the stages before it, and any wake supersedes the key's
holds. A quiet routine's hold is released by the next supervisor pass, at most
about two seconds after it is due.

Payloads no rule can read (not JSON, or a missing or non-numeric field in every
rule) wake by default, so completion and failure notices still arrive;
`"unmatched":"retain"` keeps them as events only. Process exits and source gaps
bypass the filter. A filter requires `notifications=true`, is captured at creation
and allows 1–4 rules of 1–6 stages. Its state is stored in the watch record and
survives restart, bounded to 8 keys (least recently seen evicted, sparing holds),
64 samples per key, 200-character previews, and digests for long values.

Wake on a price move over 2% at most hourly, but always on one over 5%, per symbol:

```json
{"key":"/symbol","rules":[
  [{"kind":"change","field":"/price","percent":5}],
  [{"kind":"change","field":"/price","percent":2},{"kind":"rate_limit","seconds":3600}]
]}
```

Wake when a page hash changes and stays changed for a minute:

```json
{"rules":[[{"kind":"change","field":"/hash"},{"kind":"debounce","seconds":60}]]}
```

Commit to a threat once, re-arm after three clear seconds, and pass severe events
straight through:

```json
{"rules":[
  [{"kind":"hysteresis","field":"/threat","trigger":0.8,"clear":0.4,"clear_seconds":3}],
  [{"kind":"threshold","field":"/severity","at_least":9}]
]}
```

A single threshold on an oscillating score flips on every tick; hysteresis is the
commitment. A filter only reduces wakes: a reflex loop inside a routine still
needs its own commitment logic.

## Log-driven wake selection

Watches without a notification channel select stdout/stderr lines with
`wake_policy`. All output is retained regardless.

| Policy | Wakes on |
| --- | --- |
| `{"kind":"exit_only"}` | Process exits and source gaps. Default for new watches. |
| `{"kind":"prefixes","prefixes":["event:","error:"]}` | Lines starting with a listed prefix, plus exits and gaps. |
| `{"kind":"any_line"}` | Every line, exit and gap. Explicit opt-in; watches saved without a policy keep this behavior. |

Prefixes are case-sensitive literal bytes anchored at the line start, not regular
expressions: 1–8 nonempty prefixes of at most 128 bytes without control
characters. Include every event class the agent needs; the host cannot recognize
a failure in an unselected line. Exits and gaps bypass line selection.

```json
{
  "service_id": "svc-example",
  "name": "events",
  "command": "tail -F /workspace/events.log",
  "wake_policy": {"kind": "prefixes", "prefixes": ["event:", "error:"]},
  "wake": true
}
```

Emit selected lines (or notifications) only for transitions: a dependency
becoming ready, a milestone, or an error that needs attention. Keep health
checks, status replies and routine telemetry unselected; a prefix matching every
line recreates `any_line`. Classification is workload policy: compare the
previous and new condition and emit only on change. An unknown failure may need
attention once; repeats of a known condition stay diagnostic, while recovery and
recurrence are new transitions. Do not treat every line containing `ERROR` or
`Failed` as actionable: repeated harmless warnings then wake the team. Results
already returned to the active worker should not produce another wake. Test an
emitter with repeated known warnings, a first unknown failure, recovery and
recurrence, then replay those events through the watch policies.

The service must be owned by or granted to the caller. `wake` arms a non-bot chat
once; `wake=false` does not disable an existing bot. `control_watch_wake` arms
explicitly and respects each watch's policy. Bot budgets, waits, coalescing and
single-writer admission still apply. Retrying a start returns the existing
watch's captured policy.

`control_watch_status` reports the policy, service generation and the creating
run and attempt when known. The pending count and wake cursor refer to selected
lines; the source cursor counts every frame. A watch's creator does not identify
the cause of each line.

## Reading events and receipts

`control_watch_events` returns retained frames with their original sequence
numbers and truncation flags, and `source_gap=true` when the requested range has
missing or truncated evidence (including a missing spool). Source sequence jumps
produce an eligible gap event; missing local files are reported on read. After a
gap, inspect the service's current state.

A watch owned by the attempt's chat returns a receipt in the attempt mailbox.
Pass it to `control_turn_handoff.processed` after processing the evidence; only a
successful handoff or workspace publication advances the processed watermark.
The default read starts after the last delivered or committed position; an
explicit `after` reads history. Skipped ranges and gaps need explicit
resynchronisation before publication. A foreign watch is observation only, with no
receipt. Reading alone never acknowledges.

## Score probes

Claim and message counts depend on how work is split, so they are weak progress
measures. `control_score_probe` declares how a workload scores itself: a quick,
read-only command in one of the chat's services whose last output line is a JSON
object of up to 8 numbers, such as `{"blocks_placed":75,"beds":2}`,
`{"items":120}` or `{"alerts":3}`. Earlier lines are ignored.

```json
{"name": "score", "service_id": "svc-example", "command": "cat /workspace/score.json", "interval_seconds": 300}
```

Control runs the command once on declaration (10 s bound) and rejects a probe that
produces no score. It then samples every `interval_seconds` (60 s to 1 day,
default 300) and at team milestones (a turn completes or a claim is done anywhere
in the chat's team), at most every 30 s. Samples run in the service as its user
in `/workspace` with a 20 s bound; a command still running is stopped inside the
service. A failed sample records its error and no value. Limits: 4 probes per chat
and 32 per node. Redeclaring a name replaces the probe; `clear=true` removes it.
Probes of deleted chats are dropped; a probe whose service stopped or was
replaced reports that until it is declared again.

Each probe keeps at most 240 samples (`time`, `trigger`, `values`). When full, the
older half is thinned to every second sample, so the series covers the whole run
at falling resolution while recent samples stay complete.

Operators read a team's series with `control.run.scores` (any chat in the team;
returns every probe in the root chat and its descendants, plus the milestone
count). The chat view shows each probe's latest values. Control does not
interpret names or values; they are the workload's own claims, kept for operators
and offline scorecards.

## Fresh observations

`control_observe_set` declares one observation source per chat: the latest output
of a short command in one of its running services, or a watch's newest
notification (else its latest lines). Control attaches the value at every
boundary it controls, so the agent sees current state without polling status
tools.

```json
{"service_id":"svc-example","command":"status --brief","refresh_seconds":10,"timeout_seconds":5,"max_bytes":2048}
```

Where it is attached:

- **Turn input** (every backend): at the end of each wake, after the cache-stable
  prefix (see [prefix caching](LOCAL-MODELS.md#prefix-caching)).
- **Local model steps**: the native loop refreshes it before every model request
  and sends it as the request's last message. It is never stored in the step
  transcript, so the earlier request stays a byte-identical prefix of the next.
  Each step carries the whole block, and tool results in that loop do not repeat it.
- **Tool results** (Codex and Claude Code): every Control tool result except a
  handoff and `control_observe_set` itself, as the last text item and as
  `structuredContent.observation`. When the block matches the one last shown to
  the attempt (ignoring ages), the result carries
  `[Observation unchanged (age N s)]` instead. What each attempt was shown is
  kept in memory only.

Refresh and limits:

- A new declaration replaces the source, `clear=true` removes it, and a call
  without arguments shows it with its current value. The service must be running
  and usable by the chat, rechecked at every refresh; a watch must belong to the
  chat or to a service it can use.
- A command reruns when its value is older than `refresh_seconds` (default 10,
  at most 3600; 0 means every boundary). A turn start waits at most 2 s for a
  refresh, a local step or tool result 0.25 s. Otherwise, or on timeout
  (`timeout_seconds`, default 5, at most 30) or failure, the block shows the last
  value with its age and the reason. At most one refresh per chat runs in the
  background; a timed-out command is stopped inside the service. Refreshes use no
  exec slot or execution record.
- A watch source runs nothing; it reuses the watch's latest output and notes when
  the watch has ended.
- `max_bytes` defaults to, and is capped by, `observation_max_bytes` in
  `execution.json` (default 2048, range 256–16384), cut on a character boundary.
- Values live in memory only. They never enter the objective, transcript,
  journal, recorded tool output or checkpoint; the declaration persists as chat
  configuration. After a restart the first boundary refreshes it.
- Output is untrusted program data, never instructions or authority. Keep the
  command read-only; it runs with the authority of an exec in that service.

Without an observation, when an attempt makes four consecutive status-style
`control_service_exec` calls on one service (every pipeline part is a reader such
as `ps`, `ls`, `cat`, `tail`, `pgrep` or a non-posting `curl`, optionally with
`sleep` or `echo`, and nothing is written), that result carries a one-time note
suggesting `control_observe_set`. Any other command or service resets the count.

Cost, measured with a 2 KB (about 435-token) value over eight results or steps
(ignored tests `observe::tests::measure_observation_tokens_per_tool_result` and
`local_agent::tests::measure_observation_tokens_per_step`, at 3 bytes per token):
a Codex/Claude tool result adds about 11 tokens while the value is unchanged and
about 445 when it changes. A local step prefills about 522 new tokens either way;
the stable prefix before it is reused from the cache.
