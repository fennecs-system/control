# Spawning agents and judges

`control_agent_spawn` starts background work with a plural `objectives` array:

```json
{"objectives":["Inspect the HTTP handlers","Review the tests"]}
```

Use one concise, nonblank task per child: at most 2,000 characters each and
24,000 UTF-8 bytes across the array. Shorten oversized task descriptions instead
of copying the entire conversation into every objective. Request identity is
supplied by the MCP transport; do not add a nonce or identity argument.

`control_agent_spawn_blocking` pauses the parent for one dependency or review,
with a singular `objective` and a `role`:

```json
{"objective":"Verify the proposed change","role":"worker"}
```

For shared coordination, start from a [room-linked chat](ROOM-NOTIFICATIONS.md)
before spawning. Mentioning a room in an objective does not attach it.

Give each worker ownership of its goal, resources and dependencies. Workers can
use `control_agent_message` to coordinate with same-task siblings directly;
they do not need an active parent or a root bot to relay requests and answers.
Use room notifications for evidence requiring several participants to act, and
records for observations requiring no immediate action. Keep the parent available
for user steering and consequential decisions. Assign ongoing infrastructure to
a worker if it needs persistent ownership; do not turn the steering chat into a
monitoring bot merely to coordinate the workers.

Each accepted batch reserves the shared root budget immediately, including tasks
still in the backlog, while the parent keeps working.
As space becomes available in the existing 32-pending-task execution queue, Control
materializes children from the captured source. The model-slot limit independently
controls execution concurrency. Accepting 50 tasks does not start 50 model calls.

For background work, report the handles and finish the parent turn so the user
can keep chatting. New batches queue a concise completion response in the parent
conversation, coalescing up to eight finished children per update. Callbacks use
the ordinary queue and current conversation workspace; they never restore an old
checkpoint or apply child edits. A saved receipt prevents duplicate notifications
after restart. Cancelled/revoked batches do not wake the parent, and older saved
batches retain their original polling behavior. Status tools remain available
for on-demand checks; a polling loop is unnecessary. Each completion response is
an ordinary model turn subject to the node queue and provider limits.

Private `~/.config/control/execution.json` (mode 0600) configures these separate
limits; omitted fields retain their defaults:

```json
{"max_parallel_runs":3,"max_agents_per_root":64,"max_pending_agent_intents":256}
```

Agent budgets must be positive integers up to 65,536. The MCP transport's
input-size limit also applies. Node pending-intent capacity is
shared across roots; root reservations include spawned and delegated descendants,
and are counted once rather than once again when each child materializes. Root
reservations remain consumed for that root even after completion or cancellation.
Depth remains a separate limit of two. Invalid configuration fails closed.

The response contains a `batch` ID, requested/materialized/pending counts, and the
first ten handles. Keep the identifiers distinct:

| Identifier | Use |
| --- | --- |
| `batch` | `control_agent_spawn_status` and `control_agent_spawn_cancel` |
| Child `run_id` | `control_agent_result`'s `child` argument |
| Child `chat_id` | Open the child's conversation at `/chat/<chat_id>` |

For example, inspect a returned child run with
`{"child":"<run_id>","section":"status"}`. A chat ID or batch ID is not a child
run ID; using one can produce a descendant-access error.

`control_agent_spawn_status` accepts the batch ID and optional
`offset` and `limit` (at most 20); follow `next_offset`. Repeat a page later to see
newly materialized handles. A later turn in the originating chat can inspect or
cancel the batch with `control_agent_spawn_cancel`, even after its original parent
completed. Other chats and sibling agents cannot manage it. Cancellation fences
unmaterialized tasks and stops active descendants; completed results remain.
Management preserves the originating task/improve authority boundary. Result
inspection allows at least 1,024 lineage records, scales with four times the
configured root budget, and remains capped at 262,144 records including
continuations; oversized graphs fail explicitly rather than dropping provenance.

Spawned children inherit the captured provider/model, lenses, actor and immutable
baseline from the parent's active attempt. They never read its live edits or
share its writable directory. Use `control_agent_spawn_blocking` when current edits
need a checkpoint first; a first Improve turn without a published baseline must
delegate. Source modifications and child outputs are never automatically merged.
Use `control_agent_result` with `section=status` to inspect queued/running/failed
children, and the output sections below after successful completion. Later turns
in the originating chat can inspect its recorded spawn receipts; siblings and
unrelated chats cannot. Cancellation of an active parent cascades to its children.
Room watches deliver information to existing chats and are a separate mechanism.
Batch receipts, source inputs and child IDs are checkpointed atomically. Restart
and ambiguous-acknowledgement retries do not create duplicate children. Failed
checkpoints roll back admissions/cancellation, and a full queue retains accepted
backlog without repeated state writes. Actor revocation, source grant changes and
ancestor cancellation fence future materialization. No extra model calls or
independent scheduler are used for backlog admission.

Workers can call `control_agent_spawn_blocking` with an `objective` and `role`:
`worker` or `judge`. Control checkpoints the current files, releases the worker,
runs the child, and resumes the parent with bounded feedback. The web transcript
links to the child conversation; Bots shows its execution and queue status.
Independent chats can run concurrently within the [node limit](EXECUTION.md).
A model must stop after delegation acknowledgement,
rather than poll or hold a command open waiting for a child.
Explicit delegation reserves the conversation until its dependency returns;
use spawning for independently running bots and other background tasks.

Each child has the same actor, captured lenses and authority ceiling. Its files
come from an immutable copy of the parent's paused checkpoint, including in
Improve mode. Children get independent writable workspaces; edits never merge
automatically. Feedback includes a bounded diff excerpt and a link to retained
results. The parent can use the feedback to make its own changes.

Use `control_agent_result` with the child's `run_id` as `child` and `section` set to
`summary`, `files`, or `file` to retrieve larger outputs. Follow `next_offset`
until it is null; text offsets count Unicode characters. File pages include
hashes and change/deletion metadata when a scoped baseline is available. Reads
follow the child's recorded continuations and verify published artifact hashes.
Only the requesting parent's actual delegated descendants are available; sharing
a root or actor does not grant access to unrelated chats or sibling agents.
The tool never reads live workspaces. If no viewer artifact was published, it
reports that explicitly; existing artifact size and file-type limits still apply.

Personas currently capture a role and enforced policy, not persistent autonomous
bots. A worker may delegate to depth two; a root task uses its configured total
agent budget and permits one independent judge review. Workers and judges inherit the parent's
captured provider and model. A configured alternate provider is not evidence of
available credentials. Explicit cross-model selection with availability checks
is a later feature; judge context and source access remain isolated now.

Judges have read-only project mounts and no delegation, project-access requests,
preview publishing, or administration tools. They can inspect source, use isolated
verification tools and collaborate through the linked room. Claude workspace
commands inspect `/workspace`; Codex inspects `/review` and uses `/workspace`
only for scratch and transport. `/tmp` is available for disposable verification
outputs. Their reports do not advance the development workspace.

Waiting state and child/continuation IDs are persisted together. Restart does not
create duplicate children; interrupted or failed children return that status to
the parent. Cancelling a supervisor cancels its pending descendants. A changed
parent workspace fences continuation. Merging, deployment and additional host
access remain explicit operator actions.

These are bounded node tasks using the configured providers. They are not an
unattended evaluation or judge swarm. Use provider accounts and API keys within
the applicable terms and usage limits.
