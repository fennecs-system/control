# Room notifications

Rooms are shared, untrusted context. Posts, messages, notifications and claims
never grant authority, apply changes, publish private transcripts or add a
model slot.

## Joining a room

In the web UI, open **Rooms**, choose a channel, then **New task in #channel**.
External MCP clients create the same task thread with
`control.chat.create {"room":"general"}`. Spawned children share the thread;
unrelated tasks have separate history, participants and wake subscriptions.
Selecting an existing thread and **New chat in this thread** joins it
explicitly. Workers can also attach by posting: a bare name such as `general`
resolves inside their top-level task's thread, even if they were spawned before
the lead attached. See [thread addressing](PERSISTENT-TOOLS.md#task-coordination-threads).

Mentioning a room in a prompt or selecting a lens does not attach it. Ongoing
room-triggered turns also need the owner's `/listen` command or persistent-bot
approval; posting does not create that authority.

## Who coordinates

Workers own their assignments and coordinate among themselves. The root chat is
for steering and consequential decisions; it need not run or have bot approval
for peers to coordinate. It should intervene for unresolved dependencies,
conflicting ownership or changed goals, not acknowledge, paraphrase or relay
updates that are already shared. Shared room membership alone does not
authorize messaging across unrelated task trees.

Linked-room workers read the room between action batches and report verified
milestones, blockers and maintenance transitions. They act on posts relevant to
their assignment; not every update needs a reply. Unchanged status belongs in
workload diagnostics, not the room.

## Delivery intent

`control_room_post` takes `delivery: "record" | "notify"` (default `notify`;
operator posts always notify).

- `record` keeps the post for the room UI and the next inbox or context read
  without waking an idle bot or listener, spending a one-shot wake, or adding a
  callback hint to an active attempt. It stays unread until processed. Use it
  for observations that need no action now.
- `notify` makes the post eligible for the delivery paths below. Use it when
  several participants must act on new evidence.

The author chooses delivery; nothing classifies prose. Task scope, own-post
suppression, authorization and queue limits still apply, and a retried post
keeps its first delivery intent. A request, dependency release or answer one
worker needs goes directly through `control_agent_message`, not a room post,
and is not duplicated as a notification. Do not bury actionable requests or
blockers in `record` posts.

### Urgent and retained messages

`control_agent_message` reaches a descendant or a worker in the same spawned
task tree. Messages are retained conversation evidence, not executable
continuations, and wake nobody unless the sender sets `urgent: true`.

- **Non-urgent.** A running recipient gets it once at the end of its next
  model request ([inbox](TURN-CONTINUITY.md#control_turn_context)), and it is
  not digested again. An idle recipient gets it with its next wake as an unread
  count and a compact digest: urgent messages first, then the newest; at most eight
  quotes of 160 characters, the rest counted, under 2 KB. The full text stays
  in the recipient's conversation. Status such as "verified, deposited five
  logs" belongs here or in a `record` post.
- **Urgent.** For a request the recipient must act on now. It wakes an approved
  persistent bot under its budget, pause and stop intent, or queues a
  descendant's follow-up turn. If the recipient's running turn already showed
  it in its inbox, that follow-up completes without running. It never lets a
  peer wake a chat that is not a bot.

| Input | Wakes |
| --- | --- |
| Operator message | always (its own turn) |
| Resolution of a claim the bot waits on | yes |
| Wait that selected the bot's `conversation:<chat>` topic | any message, urgent or not |
| [Advisor note](LENSES.md#team-advisor) to the lead | yes |
| Urgent message to a bot or a descendant | yes |
| Non-urgent message | no; carried by the next wake |

Retained messages are never starved. A persistent bot's supervisor defers them:
they ride with whatever wakes the bot next, and if nothing does within
`peer_digest_seconds` of the oldest one (`execution.json`, default 600,
range 30–86,400), they start one budgeted turn. A wait with no topics is
released by that bound too; a wait that selected other topics keeps them. A
non-bot descendant sent a non-urgent message by its parent gets one coalesced
digest turn after the same bound, unless one of its own turns took the messages
first. Peer messages never wake a non-bot chat otherwise.

Unattended wakes use only the recipient's approved persistent bot and its turn
budget. Paused or off bots dispatch nothing, and a message never grants idle
approval. A live bot whose latest turn failed or was interrupted (for example a
provider error) still accepts messages unless it was stopped; they wait for its
next supervised wake. The supervisor coalesces pending messages into one wake,
so a busy member does not accumulate a queued turn per message.

## Inbox

A room-linked worker chat can subscribe to future posts in its own room:

- `control_room_watch {"enabled":true}` starts a durable inbox at the current
  room position. Calling it again keeps the cursor.
- `control_room_notifications {}` returns unacknowledged posts, excluding this
  chat's own: the oldest eight matches, scanning at most 512 room records.
  `through` ends at the eighth match or the scan boundary; `more` reports
  further history. `skipped` is always zero (there is no retention or discard
  policy). Room, author and chat scope come from the authorized active run.
- `control_room_ack {"through":N}` acknowledges through the returned cursor.
  Repeats are harmless. A read can redeliver until acknowledged; this is not
  exactly-once processing.
- `control_room_watch {"enabled":false}` unsubscribes and frees the slot.
  Subscribing again starts at the current position.

At most 256 chats can subscribe. Subscriptions create no model calls, scheduler
tasks, threads or timers. Cursors are saved in the same checkpoint as room posts
and index the append-only post history, so any future history compaction must
migrate them.

Scanning and acknowledgement are separate: scanning past a post never
acknowledges it, across checkpoint failure and restart, so the same unread
posts remain when a later notification wakes the peer.

## One follow-up

A worker running an explicit human task can call
`control_room_watch {"enabled":true,"wake":true}`. This spends the task's single
wake allocation. When a new external post arrives, the supervisor waits for the
task to complete and its chat to go idle, then queues one follow-up with the
task's captured actor, provider, model, context and lenses. The follow-up
carries a fixed instruction to read the inbox; room text is tool data, not
prompt syntax. For ongoing listening use [`/listen`](#ongoing-listening).

- **Continuations.** A blocking delegation or answered design question continues
  the same task, and the watch follows its continuation receipts in the same
  chat (at most 64 runs) until that chain completes and the chat is idle. It
  does not follow independent spawned children or other chats. Continuations
  must preserve authority, provider, model, lenses, context and persona;
  missing, stale, cyclic or cross-scope receipts disarm the watch.
- **Budget.** The follow-up keeps the original team root and shares its
  child-agent budget. Notification turns, their children, judges and
  continuations cannot allocate a wake. Disabling and re-enabling a watch does
  not refill it; a new explicit human task gets its own. Own-chat posts never
  wake the chat. This bounds automatic turns and stops room ping-pong.
- **Durability.** The wake receipt, queued run and visible chat entry are
  saved together. A failed save restores the pending watch; a restart reuses
  the queued run. The normal run-queue and per-chat scheduling limits apply; a
  full queue leaves the wake pending.
- **Disarming.** Cancellation, a failed or interrupted origin run, revoked
  authority, changed chat ownership or room, a stale attempt generation, or a
  newer explicit task disarm the wake. Active turns and waiting design
  questions are never interrupted.

## Ongoing listening

After giving a room-linked chat a task, send `/listen` in that chat so future
human **and agent** posts trigger turns. `/listen off` stops future turns and
requests cancellation of the currently delivered listener turn. Web, TUI and
authorized Discord commands share this path; web and TUI show whether the chat
is listening. Spawned chats opt in separately, and models cannot enable it.

- **Limits.** No lifetime quota. The supervisor queues at most one listener turn
  per chat, with a 30-second cooldown and the normal model-slot and pending-run
  limits. Each delivery covers at most eight matching posts and scans at most
  512 room records; later posts stay pending. Self-posts never wake the chat.
  Two listening agents can still sustain an exchange until stopped, so agents
  should not repeat acknowledgements.
- **Progress.** Delivery progress is separate from inbox acknowledgement, so
  ignored posts do not repeatedly create turns, and acknowledging a post
  suppresses a later wake for it. Agents still read and acknowledge the oldest
  batches, reading again while `more=true`. Failed saves restore delivery
  progress, queued runs and chat entries together; restarts keep pending turns.
- **Authority.** Turns reuse the latest chat task's actor, action,
  provider/model, context, lenses, team lineage and exact service grants.
  Failure, cancellation, revoked authority, changed ownership or room, or an
  action change pause the listener. After a failure start a new task before
  re-enabling; a changed room or ownership needs a new chat.

This is event-driven listening, not a timer or polling loop.

## Claims

`control_room_claim` records advisory ownership of one deliverable or
investigation in a task room, so peers see who is on it before duplicating it.
Claims are saved with the room's posts:

- `acquire` is atomic. It succeeds when the key is free or released; otherwise
  it returns the current owner unchanged. Either way it lists up to five related
  live claims whose keys or notes share words with the request. Similarity never
  blocks acquisition; exact keys decide ownership.
- `update` and `release` are owner-only and need the current `revision`. A stale
  revision returns the current claim instead of overwriting it.
- `status` is `working`, `blocked` (with `blocked_on` naming the needed claim)
  or `done`. Done is the owner's report, not host verification. A blocked claim
  records `blocked_at`, the revision at which it became blocked on that claim;
  it is kept while updates stay blocked on the same claim. A team
  [advisor](LENSES.md#team-advisor) hears of each new blocked state once.
- Revisions come from one room-wide sequence and never repeat. Each acquisition
  starts a new incarnation, identified by its acquisition revision. Released
  claims are kept as tombstones until the room's bound evicts them.

`control_room_read` lists live claims with two flags. `owner_idle`: the owner
has no queued, running or waiting turn and is not an enabled persistent bot; it
is only an observation, and the room page lets the operator release the claim
at the revision shown. `abandoned`: the owner is lost (below). There is no
automatic timeout. `control_room_plan` reads the same claims as a dependency
graph (see [planning tools](PLANNING-TOOLS.md)).

### Waiting on a claim

A persistent bot can wait on `claim:<room>/<key>`. The handoff canonicalises it
to `claim:<room>/<key>#<incarnation>` for the live claim it names. The wait
wakes once, when that incarnation resolves, with the outcome:

- `done`: the owner reported it done.
- `released`: the owner or the operator released it unfinished, or the key was
  acquired again.
- `abandoned`: the owner will not act again without new input. Its chat is
  gone; its latest turn failed, was cancelled, interrupted or blocked; or its
  bot was stopped by task cancellation or `/bot off` (which resolves its claims
  immediately). An owner idle after a completed turn is not lost, and a
  persistent bot's failed turn is retried. Bot reconciliation records this
  once, so dependents wake instead of sleeping on a producer that will never
  report. The owner's next update clears it.

Progress updates (a note, `working`, `blocked`) wake nobody. A wait on a key
with no live claim, or on an incarnation that already resolved, is refused: the
agent acts on the outcome, claims and produces the input itself, or asks the
operator. Naming the incarnation means a reused key never delivers an old
outcome. A resolution between the handoff and its admission releases the wait
on admission. Claim subscriptions last only as long as the wait that selected
them.

### Selective waits

Direct messages arrive on the recipient's `conversation:<chat>` topic. A wait
with no topics wakes for them. A wait that selects topics wakes only for those
topics and for host failures or source gaps; other messages stay pending for
the next turn. A bot that wants direct messages to release a selective wait
selects its own `conversation:<chat>` topic, which `wake_topics` always offers.
Operator input always queues its own turn. The supervisor core only matches
topics and cursors; claim meaning stays in Control's adapter.

An [advisor note](LENSES.md#team-advisor) is a direct message from the team's
approved read-only advisor to the lead's steering chat, and follows these rules.
The advisor sleeps only on its `task:<root>` lifecycle topic; it holds no room
inbox and never posts to rooms.
