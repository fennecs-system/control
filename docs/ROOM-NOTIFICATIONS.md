# Room notifications

## Delivery intent

Worker `control_room_post` accepts `delivery: "record" | "notify"`.
`record` retains a useful update for the room UI and the next ordinary inbox or
context read, without waking an idle bot/listener, spending a one-shot wake, or
adding a callback hint to an active attempt. It is still unread evidence until
explicitly processed. `notify` makes the post eligible for the existing delivery
paths. Old saved posts and omitted arguments keep `notify`; operator posts always
notify. Exact task scope, own-post suppression, authorization and queue limits
still apply. Retries retain the first post's captured delivery intent.

The author chooses delivery in the tool, not by asking the supervisor to classify
prose. Record useful observations that require no action now. Send an actionable
request, dependency release or required answer for one worker directly through
`control_agent_message`; same-task siblings can message each other. Notify the
room when multiple participants need to act on new evidence. Requests and blockers
requiring action must not be buried in record-only posts. Do not duplicate direct
messages as room notifications. Unchanged status belongs in workload diagnostics.

Workers own their assignments and coordinate among themselves. The root chat is
for steering and consequential decisions; it need not run or have bot approval
for peers to coordinate. It should intervene for unresolved dependencies,
conflicting ownership or changed goals, rather than acknowledge, paraphrase or
relay worker updates that are already shared. A worker that needs an answer sends
the relevant peer a targeted callback, and that peer sends back the answer only
when it releases a dependency. Shared room membership alone does not authorize
messaging across unrelated task trees.
Peer messages are retained conversation evidence, not executable continuations.
Active workers consume them at ordinary action boundaries; unattended wakeups use
only the recipient's approved persistent bot and existing turn budget. Paused/off
bots do not dispatch queued wakes, and idle approval never comes from a message.

Admission scans and read/processed cursors are separate. Scanning past a recorded
update must not acknowledge it, including across checkpoint failure and restart.
The same unread records remain available when a later notification wakes a peer.
No semantic classifier, timer, cooldown or new supervisor is added by this policy.

In the web UI, open **Rooms**, choose a channel, then **New task in #channel**.
External MCP clients can create the same task thread with
`control.chat.create {"room":"general"}`. Spawned children share its thread;
unrelated tasks have separate history, participants and wake subscriptions.
Selecting an existing thread and **New chat in this thread** explicitly joins it.
Workers can also discover channels and attach by posting: a bare name such as
`general` resolves inside their top-level task's thread, even if workers were
spawned before the lead attached. See [thread addressing and legacy-room
compatibility](PERSISTENT-TOOLS.md#task-coordination-threads).

Mentioning a room in a prompt or selecting a lens alone does not attach it.
Ongoing room-triggered turns additionally need the owner's `/listen` command or
persistent-bot approval; posting does not create that authority.

Linked-room workers read the room between action batches and report verified
milestones, blockers and maintenance transitions. Relevant human and agent posts
require action when relevant to their assignment; a reply is not required for
every update. Unchanged status and acknowledgement loops add no work. These are
shared communication instructions, not an authority grant or proof of continuous
listening. A persistent room subscription is separate from the one-shot wake below.

Room-linked worker chats can explicitly subscribe to future posts in their own
room. Rooms remain shared, untrusted context. A notification does not grant
authority, apply changes, publish private transcripts or add a model slot.

- `control_room_watch {"enabled":true}` starts a durable inbox at the current
  room history position. Calling it again preserves the cursor.
- `control_room_notifications {}` reads unacknowledged updates, excluding this
  chat's own posts. It returns the oldest eight matching posts, scanning at most
  512 appended room records. `through` ends at the eighth matching post or the
  scan boundary, leaving later messages pending. `more` indicates more history;
  `skipped` is zero because there is no retention or discard policy. Room, author
  and chat scope come from the authorized active run.
- `control_room_ack {"through":N}` durably acknowledges through the returned
  cursor, without advancing beyond that oldest batch. Repeated acknowledgements are harmless.
  A read can redeliver until acknowledged; this is deliberately not an
  exactly-once processing claim.
- `control_room_watch {"enabled":false}` unsubscribes and releases the slot.
  Subscribing again starts at the current history position.

There are at most 256 subscribed chats. Ordinary subscriptions create no model
calls, scheduler tasks, extra threads or timers. Cursors persist in the same
checkpoint as room posts. They index the current append-only post history;
history compaction must migrate them before this history can be truncated.

## One follow-up

For ongoing listening, use the owner command below instead of this one-shot allocation.

A worker executing an explicit human task can request
`control_room_watch {"enabled":true,"wake":true}`. This spends that task's
single wake allocation immediately. When a new external room post exists, the
existing supervisor waits for that task to complete and its chat to become idle,
then queues one follow-up using the task's captured actor, provider, model,
context and lenses. The follow-up contains a fixed instruction to read the
inbox; incoming room text is tool data, not executable prompt syntax.

A blocking delegation or answered design question continues the same task.
Its existing watch follows the recorded continuation receipts in the same chat;
it waits until that chain completes and the chat is idle. It does not follow
independent spawned children or other chats, even when they share a team root.
Nonblocking children may keep working while an idle lead receives its follow-up.
Continuation edges must preserve the captured authority, provider, model, lenses,
context and persona; traversal is bounded to 64 runs. Missing, stale, cyclic or cross-scope receipts disarm the
watch. Design answers preserve the original team root and child-agent budget.

The wake receipt, queued run and visible chat entry are checkpointed together.
Failed saves restore the pending watch. A restart reuses the retained queued run
rather than creating another. The regular bounded run queue and per-chat
scheduling rules still apply. Full queues leave the wake pending.
The follow-up retains the original task's team root, so it shares that team's
child-agent budget rather than allocating a fresh quota.

Automatic wake allocation is unavailable to notification turns, their children,
judges and continuations. Disabling and re-enabling a watch does not refill its
task budget. A new explicit human task can allocate its own single wake. Own-chat
posts never wake the chat; replayed worker posts are still idempotent. These
rules bound a room conversation's automatic model turns and prevent endlessly
rearming room ping-pong.

Cancellation, failed/interrupted origin runs, revoked authority, changed chat
ownership/room, a stale attempt generation, or a newer explicit task disarm the
old wake. Active turns and waiting design questions are never interrupted. This
is a one-shot follow-up mechanism, not a persistent bot control language or an
unbounded event subscription.

## Ongoing listening

After giving a room-linked chat a task, send `/listen` in that chat to enable
future human **and agent** messages to trigger turns. `/listen off` stops future
turns and requests cancellation of its currently delivered listener turn.
Web, TUI and authorized Discord chat commands share this path. Web and TUI show
whether the chat is listening or paused; spawned chats opt in separately.

There is no lifetime reply quota. The existing supervisor queues at most one
listener turn per chat, with a 30-second cooldown and the regular model-slot and
pending-run limits. Each delivery covers at most eight matching posts and scans
at most 512 room records; later records remain pending. Self-posts never wake
the same chat. Agents should respond to relevant messages rather than repeat
acknowledgements; two enabled agents can still sustain an exchange until stopped.

Delivery progress persists separately from inbox acknowledgements, so ignored
or unacknowledged posts alone do not repeatedly create turns. Acknowledging a
post also suppresses a later wake for that already processed post. Agents still read
and acknowledge the oldest inbox batches, reading again when `more=true`.
Failed saves restore delivery progress, queued runs and visible chat entries
together; restart retains pending turns. No new thread, model polling loop or
provider transport is introduced. Idle reconciliation indexes runs once and
scans only bounded room windows.

Turns retain the latest chat task's actor, action, provider/model, context,
lenses, team lineage and exact service grants. Waiting questions or active turns
are not interrupted. Failure, cancellation, revoked authority, changed ownership
or room, or an action change pauses the listener. Start a new task before
re-enabling after failure; changed room/ownership requires a new chat.
Models cannot enable this persistent intent or replace it with a one-shot wake.
This is event-driven room listening, not a timer-driven goal or gameplay loop.

## Claims

`control_room_claim` records advisory ownership of one deliverable or
investigation in a task room, so peers can see who is on it before duplicating
it. Claims live beside the room's posts in the same checkpoint:

- `acquire` is atomic. It succeeds when the key is free or released; otherwise it
  returns the current owner unchanged. Either way it returns up to five related
  live claims whose keys or notes share words with the request. Similarity is
  advisory and never blocks acquisition; exact keys decide ownership, and the
  agent can join that effort or release its duplicate.
- `update` and `release` are owner-only and need the current `revision`. A stale
  revision returns the current claim instead of overwriting it.
- `status` is `working`, `blocked` (with `blocked_on` naming the needed claim) or
  `done`. Done is the owner's claim backed by its evidence, not host verification.
- Revisions come from one room-wide sequence, so they never repeat after a
  release. Each acquisition starts a new incarnation, identified by its
  acquisition revision. Released claims are kept as tombstones until the room's
  bound evicts them.

`control_room_read` lists live claims with two separate flags. `owner_idle` means
the owner has no active or queued turn and is not a persistent bot; it is only an
observation, and the room page offers the operator an explicit release at the
revision shown. `abandoned` means the owner is lost (see below). There is no
automatic timeout.

### Waiting on a claim

A persistent bot can wait on `claim:<room>/<key>`. The handoff canonicalises it
to `claim:<room>/<key>#<incarnation>` for the live claim it names. The wait
wakes once, when that incarnation resolves, and the wake reason carries the
outcome:

- `done`: the owner reported it done.
- `released`: the owner or the operator released it unfinished, or the key was
  later acquired again.
- `abandoned`: host facts that the owner will not act again without new input.
  Its chat is gone, or its latest turn failed, was cancelled, interrupted or
  blocked; or its bot was stopped by task cancellation or `/bot off`, which
  resolves its claims immediately. An owner idle after a completed turn, such as
  a steering chat between turns, is not lost, and a persistent bot's failed turn
  is retried. Bots reconciliation records this once, so dependents wake instead
  of sleeping on a producer that will never report. The owner's next update
  clears it.

Progress updates such as a note, `working` or `blocked` wake nobody. A wait on a
key with no live claim is refused, as is a wait on an incarnation that already
resolved: the agent acts on the outcome, claims and produces the input itself, or
asks the operator. Naming the incarnation means reusing a key never delivers an
old outcome. A resolution published between the handoff and its admission
releases the wait as soon as it is admitted. Claim subscriptions last only as long
as the wait that selected them, and operator input queues its own turn.

### Selective waits

Direct peer messages arrive on the recipient's conversation topic. A wait with
no topics still wakes for them. A wait that selects topics wakes only for those
topics and for host failures or source gaps; other messages, including direct
ones, stay pending and arrive with the next turn. A bot that wants direct
messages to release a selective wait selects its own `conversation:<chat>` topic,
which `wake_topics` always offers. Operator input queues its own turn and is
unaffected. The supervisor core only matches topics and cursors; claim meaning
stays in Control's adapter.
