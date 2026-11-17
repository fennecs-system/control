# Bounded execution

The node runs up to three model attempts at once (`max_parallel_runs`), across
independent chats. Each chat has one writer and runs its turns in FIFO order.
`/spawn` and `/btw` use separate chats and writable workspaces. A run waiting for
a delegated child or a design answer gives up its model slot but keeps its chat
reserved.

## Lanes

Delegated and spawned agents and automatic bot turns run in a background lane.
Queued conversation turns, including root-chat continuations, start before queued
background work, and background work uses at most all but one slot. A message
sent while agents are busy therefore starts at once. Turns within one chat keep
FIFO order whatever their lane. With a single slot, agents still run but wait
behind any queued conversation turn, so sustained conversation can delay them.

The queue admits at most 32 pending runs, including paused and cancelling work.

## Configuration

Node limits live in `execution.json` in the config directory
(`~/.config/control/execution.json` by default). The file must be private (mode
`0600`); omitted fields keep their defaults. Restart the node after editing it.
Invalid configuration stops startup and fails `control doctor`. Limits are never
taken from saved run state, model output or tool arguments.

```json
{"max_parallel_runs": 3}
```

`CONTROL_MAX_PARALLEL_RUNS` in the startup environment overrides
`max_parallel_runs`.

| Key | Default | Range | Meaning |
| --- | --- | --- | --- |
| `max_parallel_runs` | 3 | 1–8 | Concurrent model attempts; 1 is serial. |
| `max_agents_per_root` | 64 | 1–65,536 | Agents one root task may spawn or delegate ([delegation](DELEGATION.md)). |
| `max_pending_agent_intents` | 256 | 1–65,536 | Node-wide backlog of accepted, not yet started agents. |
| `max_advisors_per_root` | 1 | 0–8 | [Team advisors](LENSES.md#team-advisor) per root task; 0 disables. |
| `advisor_failure_streak` | 3 | 2–64 | Consecutive failed or blocked turns of one member before its advisor hears. |
| `advisor_stall_turns` | 24 | 4–4,096 | Settled team turns with no claim advancing before a stall fact. |
| `advisor_bottleneck_members` | 2 | 2–64 | Members waiting on one claim before a bottleneck fact. |
| `bot_recovery_continues` | 2 | 0–16 | Immediate recovery turns for a bot whose handoff was rejected ([recovery](TURN-CONTINUITY.md#recovery-streaks)). |
| `bot_recovery_recheck_seconds` | 300 | 30–86,400 | First backoff recheck after that; doubles per failed turn, up to one day. |
| `steering_turn_seconds` | 240 | 30–86,400 | Turn time in a steering chat before tool results suggest delegating ([steering notes](TURN-CONTINUITY.md#deadline-and-steering-notes)). |
| `steering_exec_calls` | 6 | 1–1,024 | Command executions in such a turn before the same suggestion. |
| `peer_digest_seconds` | 600 | 30–86,400 | How long a non-urgent agent message waits before a digest turn ([room notifications](ROOM-NOTIFICATIONS.md)). |
| `observation_max_bytes` | 2,048 | 256–16,384 | Size of a chat's [fresh observation](WATCHES.md#fresh-observations). |
| `local_agent_steps` | 64 | 16–1,024 | Model steps per [local-model turn](LOCAL-MODELS.md#local-agent-turns). |

## Supervisor

Admission saves the attempt's identity and captured inputs before starting its
thread. Enqueue and completion wake the supervisor through a bounded, coalescing
channel; periodic reconciliation handles recovery. Execution threads are bounded
and joined on shutdown. A panicked thread stops the node and keeps the attempt
intent for inspection after restart. Cancelling one chat does not affect others.
Results commit under the state lock; a failed save rolls back only that run and
its chat. Recovery marks unfinished attempts interrupted and keeps their
workspaces; it never silently replays model requests.

## Resource scope

Container CPU, memory and process limits apply to each attempt and its tools. The
slot limit bounds model fan-out, not disk use or a node-wide CPU or memory
reservation. There is no hard quota on live workspace storage yet; size the limit
for the machine and workload.

Model attempts and tool services have separate lifetimes:
[persistent services](PERSISTENT-TOOLS.md) keep running after a model turn ends,
while [workspace command jobs](COMMAND-JOBS.md) stop before each checkpoint.

[Spawn batches](DELEGATION.md) reserve root and node budgets when accepted. Tasks
materialize as queue space frees, and model slots separately bound how many run.
A large accepted batch does not start that many model calls at once.

## Tests

The isolated supervisor tests exercise real admission, saves and commits with
controlled workers and no provider calls. Run each alone, because they request
process-wide shutdown:

```sh
qa=$(mktemp -d /tmp/control-execution-qa.XXXXXX)
mkdir "$qa/config" "$qa/state"
env -u CONTROL_WORKER_IMAGE CONTROL_CONFIG_DIR="$qa/config" CONTROL_STATE_DIR="$qa/state" \
  nix develop --command cargo test --locked --bin control \
  worker::tests::isolated_supervisor_overlaps_cancels_orders_and_joins -- --ignored --exact
```

The same pattern runs
`worker::tests::isolated_supervisor_retains_intent_after_worker_panic`.
`worker::tests::live_parallel_workers_cancel_independently_and_shutdown_cleanly`
is an ignored live check against an isolated, authenticated Codex or Claude node
(pinned images, `CONTROL_QA_URL` and `CONTROL_QA_SERVER_BIN`): it starts two shell
commands over MCP, proves they overlap, cancels one without stopping the other,
then checks graceful shutdown, retained diagnostics and the queued follow-up.
