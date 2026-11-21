# Spawning agents and judges

Agents start other agents with two tools:

- `control_agent_spawn` starts independent background work and does not pause the
  caller.
- `control_agent_spawn_blocking` pauses the caller for one dependency or review
  and resumes it with the result.

Children share the caller's actor, captured provider and model, lenses and
authority ceiling. They start from an immutable baseline and get their own
writable workspace; their edits never merge automatically. Delegation depth is
limited to two. Budgets are set in [`execution.json`](EXECUTION.md#configuration).

## Background batches

`control_agent_spawn` takes an `objectives` array, one task per child:

```json
{"objectives":["Inspect the HTTP handlers","Review the tests"]}
```

Each objective is 1–2,000 characters, and the array is at most 24,000 UTF-8 bytes.
Write a short task per child rather than copying the conversation into each one.
The MCP transport supplies request identity; do not add a nonce.

Children inherit the immutable baseline of the parent's active attempt, never its
live edits. A first Improve turn without a published baseline must use
`control_agent_spawn_blocking` (or finish) first. Children share the spawning
chat's team room: its linked [room](ROOM-NOTIFICATIONS.md), otherwise its lead's.
Agents cannot create rooms, and naming a room in an objective does not attach it.

An accepted batch reserves the root budget (`max_agents_per_root`, default 64)
and node backlog (`max_pending_agent_intents`, default 256) immediately,
including tasks not yet started. Reservations count spawned and delegated
descendants once and stay consumed after completion or cancellation. Control
materializes children as space frees in the 32-run queue; model slots separately
limit how many run at once. Accepting 50 tasks does not start 50 model calls.

The response holds a `batch` ID, requested, materialized and pending counts, and
the first ten handles. Keep the identifiers apart:

| Identifier | Use |
| --- | --- |
| `batch` | `control_agent_spawn_status` and `control_agent_spawn_cancel` |
| Child `run_id` | the `child` argument of `control_agent_result` |
| Child `chat_id` | the child's conversation at `/dm/<chat_id>` |

A chat ID or batch ID is not a child run ID; passing one gives a
descendant-access error.

`control_agent_spawn_status` takes the batch ID and optional `offset` and `limit`
(1–20, default 10); follow `next_offset`, and repeat a page later to see newly
materialized handles. Later turns in the originating chat can inspect or cancel a
batch even after the parent turn finished; other chats and siblings cannot.
Cancelling fences tasks not yet started and stops active descendants; completed
results remain.

After spawning, report the handles and end the turn so the user can keep
chatting. Control queues a completion update in the parent chat, combining up to
eight finished children per update. Each update is an ordinary model turn on the
current conversation workspace; it never restores an old checkpoint or applies
child edits. A saved receipt prevents duplicate notifications after a restart.
Cancelled or revoked batches do not wake the parent. Status checks are for
on-demand use, not polling loops.

Batch receipts, source inputs and child IDs are saved atomically, so restarts and
ambiguous retries never create duplicate children. A failed save rolls back the
admission or cancellation. Actor revocation, source grant changes and ancestor
cancellation fence further materialization. Backlog admission makes no extra
model calls.

## Blocking delegation

```json
{"objective":"Verify the proposed change","role":"worker"}
```

`role` is `worker` or `judge`. Control checkpoints the caller's current files,
releases its model slot, runs the child from an immutable copy of that checkpoint
(including in Improve mode) and resumes the caller with bounded feedback: a short
diff excerpt and a link to the retained results. The caller's chat stays reserved
until the child returns. The model must stop after the acknowledgement rather
than poll or hold a command open. The web transcript links to the child's chat.

A root task has its configured agent budget and one blocking judge review.
Waiting state and child IDs are saved together; restart never duplicates a child,
and an interrupted or failed child returns that status to the parent. Cancelling a
parent cancels its descendants. A changed parent workspace fences the
continuation.

## Results

`control_agent_result` takes the child's `run_id` as `child` and a `section`:

- `status` while queued, running or failed;
- `summary`, `files` or `file` (with `path`) after success.

Text pages use Unicode-character offsets (default 4,096, at most 16,384) and file
listings at most 64 records per page; follow `next_offset` until it is null. File
pages include hashes and change/deletion metadata when a scoped baseline exists.
Reads follow the child's recorded continuations, verify published artifact
hashes, and never read live workspaces. If no artifact was published, the tool
says so. Only the caller's own descendants are readable; sharing a root or actor
grants nothing. A lineage walk allows the larger of 1,024 records or four times
`max_agents_per_root`, capped at 262,144; larger graphs fail rather than drop
provenance.

## Coordinating workers

Give each worker ownership of its goal, resources and dependencies, including
ongoing infrastructure it must look after. Workers message same-task siblings
directly with `control_agent_message`; they need no active parent or relay bot.
Use room notifications for evidence several participants must act on, and
records for observations that need no action. Keep the parent free for user
steering and consequential decisions rather than turning it into a monitoring
bot. Room watches deliver information to existing chats and are separate from
spawning.

## Judges

Judges get a read-only project mount and no delegation, project-access, preview
publishing or administration tools. They can inspect source, run isolated
verification builds and use the linked room. Claude and local judges inspect
`/workspace`; Codex judges inspect `/review` and use `/workspace` only for scratch.
`/tmp` holds disposable outputs. Judge reports never advance the development
workspace.

When the root chat selects the advisor lens, `control_agent_spawn` accepts
`"role":"judge"` with exactly one objective. This starts an advisor candidate
without pausing the parent, with no services and the task-level review scope. It
uses the root's advisor allowance (`max_advisors_per_root`), not its blocking
review. An operator-approved judge bot becomes the team's
[advisor](LENSES.md#team-advisor): it wakes on the `task:<root>` lifecycle
topic and on its own team's questions, and its only effect is evidence notes
sent with `control_agent_message` to the member concerned or the lead's steering
chat. It never messages judges or advisors, never posts to rooms and never sends
services. Other judges cannot run as bots or message agents.

`control_agent_spawn` also accepts `persona` (and explicit `lenses`): children
capture the [persona](PERSONAS.md)'s purpose, DNA, standing, default model and
behaviours, and a read-only persona is a judge. Without it, children inherit
the parent's captured context and model as before.

## Boundaries

Workers and judges use the parent's captured provider and model unless the
spawn names a `model` or a persona with a body. Merging, deployment and extra host access remain
explicit operator actions. These are bounded node tasks on the configured
providers, not an unattended evaluation swarm. Use provider accounts within their
terms and usage limits.
