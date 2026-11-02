# Bounded execution

The node runs up to three model attempts at once, across independent chats. Each
chat has one writer and FIFO turn ordering. `/spawn` and `/btw` use independent
chats and writable workspaces. A parent waiting for a delegated child or a design
answer releases its model slot while reserving its own chat.

Delegated and spawned agents run in a background lane. Queued conversation turns,
including root-chat continuations, start before queued background agents, and
background agents use at most all but one slot. A message sent while agents work
therefore starts at once instead of waiting for them. Turns within one chat keep
FIFO order regardless of lane. With one slot, agents still run, behind any queued
conversation turn; sustained conversation traffic can delay queued agents.

Set a node limit in `~/.config/control/execution.json` (mode `0600`), then restart:

```json
{"max_parallel_runs": 3}
```

The supported range is 1–8; 1 restores serial execution. The startup environment
variable `CONTROL_MAX_PARALLEL_RUNS` overrides the file. Invalid configuration
stops startup and fails `control doctor`. The setting is not taken from the
saved run ledger, model output or tool arguments. The queue still admits at most
32 pending runs, including paused and cancelling work.

Admission saves the attempt's identity and captured inputs before starting its
execution thread. Enqueue and completion wake the supervisor through a bounded,
coalescing channel; a periodic reconciliation handles recovery. Execution threads
are bounded and joined on shutdown. A panicked thread stops the node and retains
the attempt intent for restart inspection. Cancelling one chat does not cancel
unrelated chats. Overlapping results commit under the state lock; a failed save
rolls back only that run and its chat, preserving other committed results.

Existing container CPU, memory and process limits apply to each attempt and its
tools. This slot limit bounds model fan-out, not total disk consumption or a
node-wide CPU/memory reservation. Hard writable storage quotas remain open; size
the limit for the machine and workloads. Recovery interrupts unfinished attempts
and retains their workspaces; it does not silently replay model requests.

Model attempts and tool services have separate lifetimes. [Persistent offline
services](PERSISTENT-TOOLS.md) have independent identities, private tmpfs, resource
limits and leases; model completion does not stop them. Attempt-bound command
jobs still stop before checkpoints. Immutable Nix bases remain unchanged.

[Nonblocking agent batches](DELEGATION.md) reserve a configurable root budget
(default 64) and node pending-intent capacity (default 256). The existing queue
materializes tasks as space frees, while model slots independently bound how many
execute. A large accepted batch is not a request to run that many model calls at
once. Batch status/cancel and receipt recovery use the same durable scheduler.

The isolated supervisor test exercises real admission, saves and commits with
controlled workers, without provider calls. Run it alone because it requests
process-wide shutdown:

```sh
qa=$(mktemp -d /tmp/control-execution-qa.XXXXXX)
mkdir "$qa/config" "$qa/state"
env -u CONTROL_WORKER_IMAGE CONTROL_CONFIG_DIR="$qa/config" CONTROL_STATE_DIR="$qa/state" \
  nix develop --command cargo test --locked --bin control \
  worker::tests::isolated_supervisor_overlaps_cancels_orders_and_joins -- --ignored --exact
```

Local validation also ran
`worker::tests::isolated_supervisor_retains_intent_after_worker_panic` alone and
`worker::tests::live_parallel_workers_cancel_independently_and_shutdown_cleanly`
against isolated Codex and Claude nodes. The live check uses MCP to start two
shell commands, proves overlap, cancels one without stopping the other, then
verifies graceful shutdown, retained diagnostics and the queued follow-up.
