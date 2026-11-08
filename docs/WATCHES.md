# Watch output and wake selection

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

`control_watch_events` reads unfiltered retained frames with original sequence
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

This implements R2's watch selection seam. It does not parse game-specific
messages, add timers, or infer that an arbitrary goal
is complete. Existing watches need an explicit filtered replacement to reduce
line-driven wakes. Synthetic tests cover noisy lines, selected events, exits,
save failure/restart and missing evidence. Diamond-run improvement still requires
a controlled baseline comparison after deployment.
