# Watch notifications and diagnostic output

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
