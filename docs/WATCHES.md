# Watch output and wake selection

`control_watch_start` runs a supervised command in an authorized persistent
service. Its output is spooled independently of whether it wakes an agent.
`wake_policy` selects line events before they reach the generic bot supervisor:

| Policy | Eligible events |
| --- | --- |
| `{"kind":"any_line"}` | Every line, process exit and detected source gap. Default, including existing saved watches. |
| `{"kind":"exit_only"}` | Process exits and detected source gaps. All lines remain readable. |
| `{"kind":"prefixes","prefixes":["event:","error:"]}` | Lines starting with either literal prefix, plus process exits and detected source gaps. |

Prefixes are case-sensitive, anchored at the start, and interpreted as bytes,
not regular expressions. Supply 1–8 nonempty prefixes of at most 128 UTF-8 bytes
each without control characters. Include every event class the agent needs;
the host cannot infer a program's protocol or recognize a failure hidden in an
unselected line. Process exits bypass line selection even when the owner started
the process. A watch's creator does not identify the cause of each stream line.

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
range has missing evidence, including a missing spool or tail. Reading does not
acknowledge processing. Source sequence jumps generate an eligible gap event;
missing local files are reported on read, not through a new background monitor.
Existing retention/rotation and per-line limits still apply. Inspection of a
service's current state may be needed after a gap.

This implements R2's watch selection seam. It does not parse game-specific
messages, change the pure supervisor, add timers, or infer that an arbitrary goal
is complete. Existing watches need an explicit filtered replacement to reduce
line-driven wakes. Synthetic tests cover noisy lines, selected events, exits,
save failure/restart and missing evidence. Diamond-run improvement still requires
a controlled baseline comparison after deployment.
