# Room notifications

Room-linked worker chats can explicitly subscribe to future posts in their own
room. Rooms remain shared, untrusted context. A notification does not grant
authority, apply changes, publish private transcripts or add a model slot.

- `control_room_watch {"enabled":true}` starts a durable inbox at the current
  room history position. Calling it again preserves the cursor.
- `control_room_notifications {}` reads unacknowledged updates, excluding this
  chat's own posts. It returns at most eight posts from a window of 512 appended
  room records. `skipped` counts coalesced matching posts; `more` indicates another
  window. Room, author and chat scope come from the authorized active run.
- `control_room_ack {"through":N}` durably acknowledges through the returned
  cursor, including skipped records. Repeated acknowledgements are harmless.
  A read can redeliver until acknowledged; this is deliberately not an
  exactly-once processing claim.
- `control_room_watch {"enabled":false}` unsubscribes and releases the slot.
  Subscribing again starts at the current history position.

There are at most 256 subscribed chats. Ordinary subscriptions create no model
calls, scheduler tasks, extra threads or timers. Cursors persist in the same
checkpoint as room posts. They index the current append-only post history;
history compaction must migrate them before this history can be truncated.

## One follow-up

A worker executing an explicit human task can request
`control_room_watch {"enabled":true,"wake":true}`. This spends that task's
single wake allocation immediately. When a new external room post exists, the
existing supervisor waits for that task to complete and its chat to become idle,
then queues one follow-up using the task's captured actor, provider, model,
context and lenses. The follow-up contains a fixed instruction to read the
inbox; incoming room text is tool data, not executable prompt syntax.

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
