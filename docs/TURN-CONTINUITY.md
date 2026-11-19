# Turn continuity

How an agent keeps working across model attempts: working notes, event receipts,
handoffs, host checkpoints and recovery. Shipped scope and open follow-ups are in
the [swarm roadmap](SWARM-ROADMAP.md).

## Model

A **bot is an agent plus a supervisor plus callbacks**. Control owns task state,
evidence, permissions and recovery. A provider process may take several related
steps before it yields; the next attempt rebuilds its task from Control's
committed journal, instructions and selected history. It never resumes the ended
provider session, and provider caches are optimisations, not task state.

- Keep the provider session running while it makes useful progress. Callbacks
  deliver events and steering to that session; they are not a reason to restart it.
- Yield on a genuine dependency, an approval or design question, completion,
  cancellation or the attempt deadline. A short asynchronous operation alone is
  not a reason to yield if useful work remains.
- No periodic soft handoff timer, model sleep loops, long-poll tools or
  permanently held slots. The hard attempt deadline stays authoritative.
- A handoff supports recovery and idle waits; it is not the normal action loop.
- Token budgets are not enforced from end-of-attempt usage telemetry.

Ordinary and spawned agents use the same continuity path at their next admission.
Approving a permanent bot (or `/bot on`) only adds unattended scheduling.

## Invariants

1. One writer per chat. FIFO order, node slots, turn budgets and authority
   ceilings still apply. Idle agents release model slots.
2. Intent is durable before model or tool execution. The attempt generation fences
   all updates. Cancellation, pause, design questions and blocking delegation keep
   their usual semantics.
3. A working-state revision is promoted only together with a matching successful
   workspace publication (or an explicit paused-checkpoint transition). A draft
   does not prove its files or external effects committed.
4. Event delivery is at least once. Reading is not processing. Cursors advance
   only for validated receipts; unseen or truncated data is never acknowledged.
5. Restart interrupts an in-flight provider attempt. Model requests and ambiguous
   external effects are never replayed silently.
6. Model-written notes are attributed claims with checkpoint provenance. They
   cannot change user instructions, grants, resource truth or admitted knowledge.
7. Raw evidence and lifecycle history stay inspectable under the normal retention
   rules, even when left out of model context. Missing evidence is explicit.

## Tools

Two attempt-scoped MCP tools, advertised as `agent.continuity` (independent of
`agent.bot`), with identical semantics for every provider and spawned agent. The
worker MCP transport supplies run and attempt identity; the operator HTTP/stdio
catalog does not advertise these tools and never accepts a model-supplied identity.

### `control_turn_context`

Reads events that arrived after the turn started, from `source`
`conversation` (default) or `room`. Returns the events, the time budget
(`limit_seconds`, `remaining_seconds`; `null` when unlimited),
`continuation_enabled`, the allowed `wake_topics` and an opaque receipt;
`notes: true` adds the committed working notes. A page holds at most 16 events
and 8 KiB, with `more` and gap markers. New reads continue from the last
delivered position; `after_receipt` selects an earlier page, and a non-empty
request identity replays its exact range. Empty polls share a cursor and spend
no delivery slots; a turn may read at most 128 deliveries. Reading stages
receipts but marks nothing processed. Watch events are read with
`control_watch_events`, whose receipts go into the same mailbox.

**The wake carries its delta.** At admission a bot wake or room notification gets
one page (16 posts, 8 KiB) of its linked room's posts after the committed
watermark, rendered under the current event and recorded as the host-issued
receipt `wake`. Both come from the same state snapshot, so the receipt never
covers a post the agent was not shown. `control_turn_context` is only needed for
events that arrive after admission; tool results carry a callback hint when there
are some.

**The inbox carries new conversation.** Instructions, agent messages and results
that reach a running turn's own conversation are shown once at the end of its
next model request: a local turn's loop appends them to the newest unsent step,
and a CLI agent (Codex, Claude Code) gets them on its next Control tool result.
The sent prefix never changes. The block holds at most 8 entries and 6 KiB; an
entry over 1,500 characters is cut with an `after_receipt` that rereads it, and
the rest wait for the next request (`More: control_turn_context {}`). Showing
records an ordinary delivery, so `control_turn_context` does not repeat it and a
handoff that omits `processed` acknowledges it. Nothing is shown after a handoff
is staged. Room posts keep the hint. A CLI agent that makes no Control tool call
sees nothing until it does: Control does not drive that loop. A queued
`control_agent_message` turn whose message was delivered this way is completed
without running (unless the message shared services), so a message never also
starts a later turn; the decision uses saved deliveries and survives a restart.
See `turn_context::tools::tests` and `agent_message::tests::urgency`.

Managed sessions cannot call `control_room_notifications` or `control_room_ack`;
they read rooms with `control_turn_context {"source":"room"}` and acknowledge them
by handing off.

### `control_turn_handoff`

Only the disposition is required: `{"disposition":{"kind":"done"}}` or the bare
string `"done"`.

- **Dispositions.** `continue` requests one bounded successor because runnable
  work remains. `wait` names existing topics and an optional recheck. `done`
  stops automatic continuation of the current task; it does not cancel a bot's
  subscriptions or stop services. Bot pause is separate and needs an operator
  resume. Without an approved bot the disposition is saved for the next
  requested turn; it cannot create a supervisor or schedule a recheck.
- **`processed`.** Omitted means every receipt delivered in this attempt
  (including `wake`) that follows each source's committed position. An explicit
  list processes exactly those receipts. Receipts that cannot be applied (not
  delivered to this attempt, a gap not listed in `resynchronised`, or a range
  after an unprocessed one) stay pending and are reported in `left_pending`
  rather than failing the handoff.
- **`payload`.** Omitted keeps the latest draft or committed notes (a placeholder
  subgoal if neither exists). `base_revision` omitted or stale uses the committed
  revision; with one writer per chat it cannot move within an attempt, and
  publication rechecks it.
- **Wait topics** are matched to current `wake_topics` and never widened beyond
  them. `room:general`, `general` and `#general` all select `room:general`; a
  bare kind such as `conversation` selects its subscriptions; unknown names are
  dropped. If topics are omitted, or a non-empty list matches nothing, or more
  than 16 match, the wait keeps every current subscription (own conversation
  first, then rooms, other topics, watches; truncated to 16). An explicit `[]`
  waits only for urgent failures and direct messages.
- `claim:NAME` waits on a claim; an already resolved claim records `continue`.
  `port:PORT_ID` waits on a pending port request: the operator's decision,
  approved or denied, queues the requester's next turn ([ports](PORTS.md)); an
  already decided one records `continue`. Without continuation (no approved bot)
  a recorded `continue` would start no turn, so such a wait is refused instead
  and the agent acts on the resolution in the same turn.
- Arguments that do not deserialize are refused naming the failing field: a
  `payload` needs all of `subgoal`, `findings`, `tried`, `pending` and `next`.
- `recheck_seconds` omitted or `0` means events only; a positive value is a
  fallback recheck. An over-long `progress` (over 1,000 characters) is truncated.
- Every adjustment is listed in `adjusted`. Only the accepted draft is kept.

Payload validation still refuses, because notes are claims and are never silently
rewritten. Limits: 12 KiB of UTF-8 JSON, 16 findings, 16 tried approaches,
16 pending actions, 8 next actions and 1,000 characters per text field. Each
refusal says how to retry, including retrying without a payload.

The same idempotency identity with the same payload returns the same result;
conflicting reuse fails. A failed save is not an acknowledgement.

**Handoff is an explicit yield.** Once the draft is durable, the host stops
attempt-owned writers and commits the workspace, notes and processed cursors in
the worker's finish transaction, without waiting for another model reply. Until
the attempt ends, only observations and handoff corrections may run; Control's
typed tool policy enforces this, not the tool's own read-only annotation.
Persistent services survive the yield; remaining attempt-owned jobs are
cancelled, so work that must outlive a turn belongs in a persistent service. The
MCP fence does not cover a native CLI's private built-in shell; container
isolation is the boundary there. Cancellation and shutdown still override
publication.

### Refusals name a fix

Every refusal from the turn, service, room, watch and agent-message tools names a
concrete next call (tool and JSON arguments) or says "ask the operator via
control_design_ask". Worker dispatch appends the family's default fix to any
refusal that lacks one. `worker_tools::fixes::tests` scans those tools' sources
for error strings and checks each reaches the model with a fix.

A `control_service_exec` command that finishes within `wait_seconds` returns its
result marked `final: true`; only a still-running result carries a `wait` hint
naming the exact `control_service_exec_status` call.

## Working state

Model-authored notes hold the current subgoal, findings (text claims with
confidence), tried approaches and outcomes, pending actions and expected results,
and next actions conditional on results. Control supplies identity, checkpoint and
instruction provenance; the model cannot forge them. The original assignment and
later steering are kept as Control-owned references and are never replaced by the
model's summary. Unknown fields and schema versions are rejected.

## Events and wakes

Ingestion, delivery and processing are tracked separately. The supervisor's
ingestion cursor only deduplicates; delivered ranges belong to the attempt and
processed watermarks to the chat. On a committed handoff, pending wake reasons are
retired only up to the committed watermarks; newer events stay pending. Overflow
needs an explicit resync receipt; a truncated preview never consumes a range.

- **Watch wake policy:** any line (default), exit only, or 1–8 literal line
  prefixes of at most 128 bytes plus exit. All output stays in the spool. No
  regular expressions or domain parsing in Control; task containers can print
  explicit milestone markers.
- **Waits narrow, never widen.** A committed wait selects which existing topics
  wake the bot. Deferred room posts stay readable. Human input, cancellation,
  approval answers, direct task messages and resource failures (urgent watch
  exits and gaps) bypass waits. Missed timer ticks are not replayed.
- **Fresh bot activation** starts at the current room tail. Reconfiguration,
  pause/resume and reload keep that origin. Older posts remain readable with
  `control_room_read` but do not wake the bot.
- **Continue** creates at most one successor per predecessor and working-state
  revision. Pending events join it; it counts against the normal cooldown budget,
  and user turns keep their FIFO priority.
- A bot session that publishes without a handoff may resume once through the
  normal supervisor budget. Explicit wait/done, unanswered approvals, newer input
  or failed publication prevent this; completion is never inferred from prose.

A watch's processed watermark is retired only when its record is deleted, since
watch ids are never reused. After a publication, a bot chat's watch records are
deleted once terminal, fully offered, processed through their last sequence and
with no turn of the chat queued or active.

## Commit and recovery

The worker's finish transaction quiesces command writers, publishes the workspace,
then under the state lock checks the attempt generation and checkpoint parent and
promotes the handoff, processed watermarks, workspace head, run result and
continuation intent together, with the usual rollback. Other chats' progress is
never reverted. Paused design/delegation checkpoints use the same association; a
child completion is evidence, not permission to overwrite the parent's notes.

On a crash before commit, the last committed handoff and cursors remain. The draft
is kept as interrupted evidence, not as committed fact. External effects are
reconciled through service/job intent receipts, never by blind replay or by
rolling services back. Uncommitted event ranges stay as backlog; a restart alone
does not retry the failed attempt.

### Recovery candidates

After confirming attempt writers stopped, the host keeps the stopped attempt's
workspace as an immutable recovery candidate, with its generation, base, project
grant, termination cause and accepted draft. It uses the checkpoint copy rules
(credentials, generated directories and symlinks excluded). Stop, copy and save
failures are explicit. Restart reconciliation captures interrupted work too.

`control_recovery` lists candidates, reads their notes and files (UTF-8 files up
to 64 KiB), and with `restore: true` copies one into a new
`.control-recovery-GENERATION` folder in the workspace, only if the original base
and project grant still match. It never replaces the committed head or
acknowledges events. A candidate alone never schedules a turn.

### Host checkpoint

When a managed turn hits its time limit, its local step cap, or ends on a provider
error before handing off, the finish transaction records `continuity.interrupted`:
generation, cause, whether a recovery candidate was captured, elapsed time, any
staged draft's progress and the last 8 tool calls (200 characters per field). It
is built from host records without a model call. A local model's turn adds its
latest step's reasoning, verbatim (the last 1500 characters), and the next turn
is told to continue that plan ([Thinking](LOCAL-MODELS.md#thinking)). The next turn's working notes
render it with the restore call `control_recovery {"generation":"…","restore":true}`.
It advances no head or cursor and is cleared by the next published handoff.

### Recovery streaks

Each attempt has a new generation, so the chat counts turns that ended without an
accepted handoff since the last published one, and keeps the last handoff
rejection (a storage failure is not a rejection). For a persistent bot:

- A rejected handoff, or a turn cut off by its time limit, step cap or provider
  failure, gets an immediate recovery turn, at most `bot_recovery_continues`
  times per streak (default 2, range 0–16). A rejection wake quotes the error and
  asks for a repaired receipt; for an unseen or unbaselined range it says how to
  resynchronise (re-read each source from its committed position and list every
  receipt, oldest first).
- A turn that ended without calling the handoff is waiting, not runnable, and is
  never answered with `continue`.
- Otherwise the bot waits on all its subscriptions with a recheck after
  `bot_recovery_recheck_seconds` (default 300, range 30–86,400), doubling with
  each further failed turn up to 64 times that or one day.
- Past the allowance, once per streak, the node notice, the bot status
  (`cannot checkpoint`) and the team advisor report it with the last rejection.
- A published handoff ends the streak.

See `bots::tests::recovery` and `bots::tests::handoff_cost`.

A timed-out turn fails like any other. Claims of an owner that is not an approved
persistent bot are marked abandoned so dependents wake; the owner keeps them and
can resume one with `update`, which clears the flag. The team advisor hears of it
once (`timed_out`). The next wake says the previous turn hit its limit, points to
`control_recovery` and the claims, and suggests splitting or delegating the work.
See `bots::tests::timeout`.

## Turn limits

Each attempt captures its limit at admission. `0` or `none` means unlimited;
other values are clamped to 60 s–24 h.

| Attempt | Default | Setting |
| --- | --- | --- |
| Explicit operator limit (wins for every attempt) | none | `worker-timeout-secs` / `CONTROL_WORKER_TIMEOUT_SECS` |
| Local backend (`local:NAME` chat, spawn or bot) | 3,600 s | `local-turn-timeout-secs` / `CONTROL_LOCAL_TURN_TIMEOUT_SECS` |
| Chat that is an approved persistent bot | 1,200 s | `bot-turn-timeout-secs` / `CONTROL_BOT_TURN_TIMEOUT_SECS` |
| Anything else (including temporary or stopped bots) | 600 s | — |

The setting files live in the node configuration directory. The limit depends on
the chat, not the run kind: every run in an approved bot chat uses the bot limit.
Approval during an active attempt switches only the implicit default. The
deadline warning, the context budget, the service-exec wait cap and the host
checkpoint all use the same limit (`attempt_control::deadline`). Quiet output is
not a health failure, and there is no automatic extension. Command deadlines
owned by the attempt are capped by its deadline; responses state requested and
effective limits. Persistent service jobs are independent of it.

Local turns also have a model-step cap, `local_agent_steps` (default 64, range
16–1,024). The agent is warned once when few steps remain; reaching the cap ends
the turn with cause `step_limit`, writes the host checkpoint and is resumed like
a timeout ([local agent turns](LOCAL-MODELS.md#local-agent-turns)).

### Deadline and steering notes

Tool results other than `control_turn_handoff` can carry two advisory notes, each
at most once per attempt and never after a handoff is staged. The attempt records
which were sent (`nudges`).

- **Deadline:** when the remaining time is at most 15% of the limit or 90 s,
  whichever is larger (half the limit for limits under 180 s):
  `N s left in this turn: publish your checkpoint (control_turn_handoff) now and
  continue in a routine or the next turn.`, followed by the request to record key
  findings and the current plan in the working notes (`payload.findings`,
  `payload.next`), since an omitted payload keeps the older notes. The local
  loop's context and step-cap notes ask the same
  ([local agent turns](LOCAL-MODELS.md#local-agent-turns)). Unlimited attempts
  get none.
- **Steering:** when the chat steers active members (chats spawned from it,
  directly or through members, with a turn queued, running or waiting, or
  approved as persistent bots) and its turn has run `steering_turn_seconds`
  (default 240) or made `steering_exec_calls` (default 6) calls to
  `control_service_exec`, `control_workspace_run` or `control_workspace_start`:
  `You are steering N active members: delegate this work to a member and end
  your turn.`

The notes stop nothing and grant nothing. Commands run inside a provider's own
shell are not counted. See `worker_tools::nudges`.

## Context projection

Lifecycle records stay in storage and the UI, but raw `Bot woke: …` entries are
left out of managed prompt history. Automatic turns are identified by typed run
metadata, never by matching prose; failures, approvals and human instructions are
never hidden with them. The current wake appears once as a delta envelope.

Projection order: policy and original/steering instructions; committed working
state with provenance; current deltas; selected task history and lenses; the
current request.

Required Control framing (adapter policy, original assignment, working notes,
lenses and current request) has a 256 KiB guard. If it cannot fit, admission
stops with an actionable context-budget error rather than truncating
instructions. History selection belongs to the configured compactor (Recent,
Passthrough or Lens; see [lenses](LENSES.md)), and assembly keeps its selection in
order without a second truncation pass. The guard does not cover provider system
prompts, tool schemas or later tool responses.

At admission Control builds a typed `PreparedContext` and saves its exact
rendered text as a private attempt artifact before launch; adapters send that
captured text. This records what was projected; it does not expose provider
reasoning.

## Code map

| Area | Responsibility |
| --- | --- |
| `crates/context` (`working_state`, `turn_mailbox`, `turn_context`, `tool_policy`, `agent_policy`) | Typed contracts, validation, pure projection, the after-handoff tool fence and managed guidance |
| `src/turn_context.rs`, `src/turn_context/tools.rs` | Tool host, receipts, handoff normalisation, host checkpoint |
| `src/bots.rs`, `src/bots/lifecycle.rs`, `crates/bot/src/supervisor.rs` | Watermarks, pending reasons, waits, continuation, recovery streaks |
| `src/watches.rs`, `src/watches/policy.rs`, `src/rooms/notifications.rs` | Delta ranges, wake policies and gaps |
| `src/worker.rs`, `src/attempt_control.rs`, `src/recovery.rs` | Finish transaction, deadlines, recovery candidates |
| `src/worker_tools/fixes.rs`, `src/worker_tools/nudges.rs` | Refusal fixes and advisory notes |
