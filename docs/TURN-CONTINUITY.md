# Turn continuity implementation handoff

Design for Sol, 2026-11-04. The implementation checkpoint below is historical;
see the [current roadmap](SWARM-ROADMAP.md) for shipped scope and open follow-ups.
Provider-session contract clarified 2026-11-06.

The next priority is reducing stop/start context pollution: longer active slices,
event deltas, and reliable working-state handoffs, while preserving Control's
recovery model. Implement this in small reviewed slices. Keep the current
playtest running unchanged while establishing its baseline.

Control owns task state, evidence, permissions and recovery. A provider process
may perform several related steps before yielding; the next process reconstructs
its task from Control. Provider session reuse, API/local-model adapters and
background summary generation are outside this implementation.

The runtime model is **bot = agent + supervisor + callbacks**. While meaningful
activity is advancing the goal, preserve the active provider session within its
attempt. If that attempt ends with runnable work remaining, the supervisor may
admit a fresh attempt without another human "continue", subject to existing
authority and budgets. Every new attempt reconstructs context from Control's
committed journal, instructions and selected history; it never resumes the ended
provider session. Provider caches are optional optimizations, not task-state
authority. A real dependency or a scheduled condition is
different from runnable work. Neither callbacks nor a short elapsed interval
justify periodically restarting an agent.

### Implementation checkpoint

Scope correction: agents own handoffs, callback delivery and context; bots add
supervisor admission and approved unattended scheduling.
Keep the existing append-only chat tape, comonad positions and configured
compactor/lenses. A task view may omit typed wake bookkeeping without deleting
history or keeping every old instruction in the model prompt forever. General
context browsing, new evidence expansion and a new scheduling language are not
prerequisites for the next continuity playtest. Do not grow those features while
the core lifecycle remains incomplete.

The feature branch contains the private projection and captured-input
path, typed working notes, attempt-scoped deliveries/drafts, atomic publication,
conversation/room delta prototypes, host-tool yield fencing, and a budgeted,
deduplicated runnable-successor path. Callback hints can reach an active session
on normal MCP responses. The prompt guidance no longer demands termination after
each event. Ordinary and spawned agents use this same path at their next admission.
Committed idle waits narrow existing topics, preserve operator timers, wake for
process exits and have a bounded recheck. Active sessions are never restarted
just because those callbacks arrive. Captured inputs reuse the append-only
journal component; finished handoff receipts leave the mutable state checkpoint
with finished tool activity instead of being rewritten on every save.

Approving a permanent bot or using `/bot on` enables unattended scheduling; the
agent already owns its continuity. Pre-existing provider attempts without a
mailbox retain their protocol until the next admission. Captured typed
triggers identify why that turn exists; conversation and room reads issue exact
delivery receipts. Managed room processing uses handoffs instead of the ordinary
room-ack tool. A successful published bot session that omits a handoff can resume
once through the normal supervisor budget, retaining its last report. Explicit
wait/done, unanswered approvals, newer input and unsuccessful/interrupted
publication prevent that fallback; it does not guess completion from prose.

The first implementation deliberately supports bounded text working notes and
conversation/room receipts. Unsupported evidence/resource reference types are
absent from the core and tool schemas. Saved notes from #133 can still decode
its empty reference arrays; those placeholders are omitted on subsequent saves.
Off-lock watch receipts and watch
prefix/exit selection remain outside this slice. Existing cursor-based watch tools remain
available. Idle waits can ignore telemetry while retaining it until a relevant
event or recheck. MCP yield fencing does not intercept a native CLI's private
built-in shell; container isolation remains authoritative there. This branch is
not deployed and has not replaced any current provider session or service.

Fresh bot activation starts at the current room tail, even if the chat has an
older inbox. First room linkage records the same origin in the session, active
mailbox and supervisor ingestion cursor. Reconfiguration, pause/resume and reload
preserve the origin. This excludes prior playtests from automatic delivery and
wakes; it does not delete posts or prevent explicit `control_room_read` browsing.
Empty deltas return a reusable source-position receipt. Repeated idle reads at
that position neither consume additional delivery slots nor rewrite state.
Fresh reads default to the last delivered position, so callers receive new
events without repeatedly injecting earlier batches. Delivery is still separate
from processing; only committed handoff receipts advance the room inbox.
Processing a delivered steering message retires its queued run without suppressing
the handoff's requested continuation; genuinely newer input still takes precedence.

## Evidence and diagnosis

The inspected Cass chat is `c-18e48bae83f5c340-7`. A captured state snapshot had
49 runs, including 45 bot wakes and 48 completed runs. In the completed interval
2026-11-03 13:44:24–13:54:24 UTC, runs `18e48ce47f50b5c0-171` through
`18e48d701c514228-201` comprise 17 consecutive monitoring turns:

- Every turn called room notifications, watch events, service status and room
  acknowledgement: 68 calls, with no gameplay action or room post in this interval.
- Recorded attempt time totals 313,933 ms. Provider telemetry totals 1,711,818
  input tokens, including 1,362,304 cached input tokens, and 3,730 output tokens.
  These are aggregate attempt usage, not the size of one prompt or a billing claim.
- Reconstructing the next turn's default Recent window from saved messages and
  journal replies yields 19,311 characters across 24 entries. Those 17 monitoring
  exchanges occupy 10,026 characters, about 52%. Wake-message text occupies
  14,810 characters across the entire window. The calculation excludes framing,
  lenses and the separate handoff; it is not a captured provider prompt.

Alice's repeated joins/leaves and progress posts wake Cass after Cass has reached
a stable connection. Cass checks itself, acknowledges Alice's news, and ends.
Earlier turns did advance the protocol diagnosis; the whole run was not idle.
The original playtest also explicitly requested one action per bot turn and an
end at asynchronous boundaries. That instruction contributes to fragmentation;
do not silently override it in the live test or attribute all latency to the
harness. The next comparative test must explicitly permit longer active slices.

The code explains the feedback loop:

- `src/bots.rs::queue` appends a wake message containing recent watch output.
- `src/conversation.rs::Entry::render` treats that message and its reply as
  ordinary prompt history; `Recent` retains by recency and character count.
- `handoff` adds the previous attempt's last six eligible observations, so the
  monitoring ritual becomes both recent conversation and execution handoff.
- `crates/bot/src/supervisor.rs` coalesces transport events but does not decide
  their relevance. Reading/acknowledging room content does not consume the
  supervisor's independently queued reasons. Events already handled during a
  running turn can consequently still cause another turn.
- `src/watches.rs::observe` retains the last five lines, not just the new range;
  wake descriptions can repeat old output alongside a new line count.

The implementation must reduce unnecessary model admissions as well as prompt
size. Summarizing the ritual alone would still pay for the ritual.

## Required invariants

1. One writer per chat, existing FIFO, node slots, turn budgets and authority
   ceilings remain enforced. Idle agents release model slots.
2. Intent is durable before model/tool execution. Attempt generation fences all
   updates. Cancellation, explicit pause, design questions and blocking delegation
   retain their existing semantics.
3. A working-state revision is promoted only with a matching successful workspace
   publication, or an existing explicit paused-checkpoint transition. A draft is
   not proof that its files or external effects committed.
4. Event delivery is at least once. Reading does not mean processing. Cursors
   advance only for validated receipts; never acknowledge unseen/truncated data.
5. Restart interrupts an in-flight provider attempt as today. Never silently
   replay model requests or ambiguous external effects. A later explicit retry
   or independently admitted wake uses committed state and retained evidence.
6. Model-written facts remain attributed claims with checkpoint provenance. They
   cannot alter user instructions, grants, resource truth or admitted knowledge.
7. Raw evidence and lifecycle history remain inspectable under existing retention
   rules, even when omitted from model context. Missing evidence is explicit.

## Bot = agent + supervisor + callbacks

Keep the agent's provider session active while useful work is progressing. The
supervisor owns lifecycle and recovery; callbacks supply events and steering to
that active session. A callback is not a request to restart the agent. Within
an existing model attempt, the agent can inspect,
act, verify and take the next related action. Remove Control-generated guidance
that requires one action and immediate termination at every watch start.
Keep explicit user-imposed limits effective and visible.

Consume newly available events at meaningful action boundaries through the
context tool below. Do not create a follow-up run solely because an event arrived
while the chat's writer was active. Do not inject arbitrary text into a provider's
stdin after launch or depend on native session-resume protocols.

Yield when there is a genuine unresolved dependency, an approval/design question,
task completion, cancellation, or the configured attempt deadline. A short asynchronous
operation does not by itself require yielding if independent useful work remains.
Do not add model sleep loops, long-poll tools or a permanently occupied slot.

Do not introduce a periodic soft handoff timer. Report elapsed/remaining allowance
under the configured hard attempt deadline, which remains authoritative. The
agent should checkpoint before that boundary when practical, but useful progress
must not be interrupted merely because a short interval elapsed. Durable handoffs
support recovery and genuinely idle agents; they are not the normal action loop.
Do not promise token-budget enforcement from end-of-attempt CLI usage telemetry.

## State and tool contract

Add a narrow `src/turn_context.rs` adapter and pure data/validation/projection
types in `crates/context`. Extend existing bot transitions only where mailbox
receipts and self-continuations require it. Avoid a broad crate extraction.

| Record | Contents and ownership |
| --- | --- |
| WorkingState | Schema version, monotonic revision, chat, originating run/attempt generation, published checkpoint reference; model-proposed goal progress, findings, rejected approaches, pending work and conditional next actions |
| Instruction references | Control-owned original assignment and later user/authorized steering references, in order; never replaced by the model's summary |
| EventReceipt | Opaque ID tied to chat/run/attempt generation and exact delivered per-source ranges; records gaps and pagination |
| PendingHandoff | Attempt-scoped candidate WorkingState, base revision, acknowledged receipt IDs, disposition, optional task-progress summary, and idempotency identity |
| ContextTrace | Projection version, exact Control text sent to the harness, section sizes and selected/omitted evidence references; privately retained with the attempt |

Working-state fields include: current subgoal; findings with confidence/uncertainty
as text claims; tried approaches and outcomes; pending actions and expected results;
expected event conditions; and next actions conditional on results. Control fills
identity, checkpoint and instruction provenance; the model cannot forge them.

Start with a 12 KiB UTF-8 JSON limit for the model-authored working-state payload,
16 findings, 16 attempted approaches, 16 pending actions and 8 next actions.
Reject oversized writes with actionable errors;
do not silently truncate claims or instructions. Cap each free-text field at
1,000 characters within the total limit. Schema versions and unknown fields
must be validated, not accepted as arbitrary executable configuration.

Expose two attempt-scoped MCP tools through the shared registry and BotOps-style
host boundary. Both providers and ordinary spawned agents receive identical
semantics. They are advertised as `agent.continuity`, independently of `agent.bot`.
The worker MCP transport supplies run/attempt identity; the operator HTTP/stdio
catalog does not advertise these attempt-bound tools or accept model-supplied
attempt identities. This shared transport is the integration boundary for native
adapters, not a new public session-authentication protocol.

- `control_turn_context`: returns committed working state (or this attempt's
  staged candidate), conversation/room deltas, remaining attempt time,
  `continuation_enabled`, allowed wake topics and opaque delivery receipts.
  Accept an optional previous receipt for pagination, including an empty read's
  source-position receipt. Nonempty request identities replay their exact range;
  an empty poll is a fresh observation and shares its cursor with other empty polls.
  Default delta page: at most 16 items and 8 KiB, with `more` and explicit gap
  markers. A read stages delivery receipts but does not acknowledge processing.
- `control_turn_handoff`: accepts the base working-state revision, candidate
  state, processed receipt IDs, disposition and optional progress summary. It
  validates scope/generation, saves PendingHandoff and returns its revision.
  Same idempotency identity and same payload returns the same result; conflicting
  reuse fails. A failed save is not an acknowledgement.

Dispositions are `continue`, `wait`, `done`. `continue` requests one bounded
successor because runnable work remains after the slice; `wait` specifies existing
sources/conditions and an optional recheck deadline; `done` relinquishes automatic
continuation. Explicit bot pause remains distinct and requires operator resume.
`done` stops synthetic continuation for the current task; it does not revoke an
operator's bot subscription or stop services. Maintenance belongs in `wait` with
explicit conditions. The optional progress summary is task progress, not a
mandatory wake/sleep report.
Without an approved bot, dispositions are saved with the agent's workspace for
its next requested turn; they cannot create a supervisor or schedule a recheck.

After a handoff is accepted, the provider should finish naturally. Fence further
mutating tools for that attempt, allowing only reads and correction of the draft;
this prevents actions after the declared checkpoint candidate. Do not implement
yield by killing the process and treating cancellation as success. If the CLI
fails to finish, normal timeout/cancellation applies and the candidate remains
uncommitted. Forced graceful provider termination is outside the first slice.
Use Control's typed tool policy for that fence, not caller-supplied read-only
annotations. A tool with an effect cannot bypass it by labeling itself a read.

## Events recorded versus events requiring a model

Keep source ingestion, delivery and processed cursors separate. The current
supervisor cursor deduplicates ingestion; it is not a model acknowledgement.
Track delivered ranges on the attempt, and committed processing watermarks on
the chat/bot. Protect outstanding referenced watch evidence from retirement.

At dispatch, record the initial delta receipt with the attempt intent before
launch, retaining the full structured wake reasons and source ranges. The
supervisor currently removes pending reasons on dispatch; its held topic names
alone are insufficient to recover an unprocessed delivery. During an active
slice, context reads page additional events using the
same receipt mechanism. Include current authoritative status for relevant handles
without dumping historical service execution lists. Additional status tools
remain available for explicit investigation.

On successful handoff commit, retire pending wake reasons only through the
committed watermarks. If a coalesced reason includes newer events, retain it and
deliver only the unprocessed source range next time. A later event racing with
commit must remain pending. Overflow records require an explicit resync receipt;
never interpret a truncated preview as consumption of a complete range.

For managed turns, legacy `control_room_ack` must stage a processing receipt in
the same transaction model rather than independently clearing future wake work.
Ordinary room listeners and legacy bots retain their existing contract. Watch
reads stay non-acknowledging. Do not wire a plain status read to wake suppression.

Prevent passive telemetry from waking an idle model on every line:

- Add an optional watch wake policy: any line (legacy default), exit only, or
  bounded literal line prefixes plus exit. Retain all output in the spool. Limit
  to 8 prefixes of 128 bytes; no arbitrary predicates, regular expressions or
  domain-specific parsing in Control. Task containers can emit explicit milestone
  markers. Terminal failures and source loss remain actionable.
- A committed wait may narrow which already-authorized topics wake that bot.
  Deferred room news remains readable and can be batched at its next actionable
  wake or finite recheck deadline. Do not infer relevance by keyword or silently
  unsubscribe the bot. Include the effective wait in operator status.
- Human input/steering, cancellation, approval answers, authorized direct task
  messages and resource failure bypass ordinary telemetry deferral. Preserve
  their existing ownership and FIFO semantics; they do not gain new authority.
- Initial maximum deferred-room recheck is five minutes, bounded by an existing
  shorter approved timer. Model requests cannot extend the operator's timer or
  grant subscriptions/resources. Explicit user directives to process every event
  remain effective. A timer is a fallback, not a reason to repeat status rituals.

When a turn commits `continue`, create at most one durable successor intent keyed
by predecessor and working-state revision. Existing pending eligible events join
that successor; do not create separate event and continuation runs. Count it in
the normal cooldown/window budget and preserve background scheduling. Materialize
it when queue capacity permits. User turns remain ahead according to current FIFO;
invalidate/re-evaluate a stale continuation intent if newer work changes its
checkpoint. Temporary one-shot bots retain explicit rearm semantics.

## Commit and recovery

Extend the existing worker finish transaction; do not publish working state from
the tool handler. First quiesce command writers and publish the workspace using
existing rules. Under the state lock, validate current attempt generation and
checkpoint parent, then promote handoff, processed watermarks, workspace head,
run result and continuation intent together. Persist the journal/reference and
state with existing rollback/recovery conventions. Include the newly touched
bot/chat fields in rollback without reverting another chat's progress.

The same atomic association applies when existing design/delegation machinery
publishes a paused checkpoint. Transfer handoff lineage through its explicit
continuation receipt; do not release the bot or duplicate a successor while that
chain still owns the chat. A child completion is evidence, not permission to
overwrite the parent's working state.

On crash before commit, retain the last committed handoff and cursors. Retain the
draft as interrupted evidence with its attempt/checkpoint provenance; do not
present its claims as committed facts. External actions may already have happened:
use existing service/job intent receipts and ambiguous-result handling, not blind
replay. Do not roll back external services to match the old handoff.
Retain uncommitted initial and subsequent event ranges as recovery backlog. A
restart must not turn that backlog alone into an automatic retry of the failed
attempt; include it when an explicit retry or a new independent wake is admitted.
Expose that recovery condition in status rather than losing the dispatch reasons.

A provider ending successfully without a handoff keeps its result/evidence and
workspace under existing rules, marks continuity incomplete and retains the last
committed working state. It does not acknowledge delivered events or invent a
`continue` disposition from prose. Tag the retained state's older checkpoint and
require reconciliation before actions depending on it. Existing event budgets
bound repeated fallback turns; expose the missing handoff for diagnosis.

## Context projection and inspection

Keep lifecycle records in storage and UI, but exclude raw `Bot woke: ...` entries
from managed prompt history. Do not hide failures, approvals or human instructions
along with bookkeeping. Use typed run/entry metadata, not matching result prose,
to identify automatic turns. Include only their explicit task-progress summaries
in the task-history section; show the current wake once as a delta envelope.

Projection order: policy and original/steering instructions; committed working
state with provenance; relevant current deltas/resource changes; selected task
history and lenses; current request. Use the structured handoff instead of blindly
reinjecting the previous six status observations for managed turns. Preserve the
legacy projection only for already running attempts without a captured mailbox.

Required Control framing has a 256 KiB guard, including adapter policy, original
assignment, working notes, lenses and current request. History selection belongs
solely to the configured `Compactor`: Recent supplies its bounded window,
Passthrough supplies all selected task history for provider-native compaction,
and Lens supplies its chosen verbatim entries and summary blocks. Assembly
preserves that selection in chronological order without a second truncation
pass. This does not bound provider system prompts, tool schemas or later tool
responses. Callback pages retain their separate byte/item bounds.
If required framing cannot fit, stop admission with an
actionable context-budget error, not silent truncation or instruction loss.
Do not replace a human constraint with an unchecked model summary.

Build a typed PreparedContext at admission and save its exact rendered text as a
private attempt artifact before launch; adapters must send that captured text.
Record trace references atomically with attempt intent, recover orphan writes
using existing artifact patterns, and apply retention/access rules. Capture later
context-tool envelopes by receipt too. This supports comparing what was available
with what was actually projected; it does not expose hidden provider reasoning.

## Implementation sequence and code map

1. **Projection and measurement:** add typed automatic-turn classification and
   PreparedContext/trace; exclude repeated wake text while preserving raw history,
   required instructions and meaningful progress. Add the sanitized Cass fixture.
2. **Working state and receipts:** add optional versioned state, the two MCP tools,
   evidence resolution and transactional commit/rollback. Add replay/crash tests
   before enabling event-consumption suppression.
3. **Longer slices and deltas:** change managed guidance, expose current deltas
   during execution, connect committed receipts to supervisor pending reasons,
   and implement bounded deduplicated continuation intents and wait disposition.
4. **Wake selectivity:** add watch prefix/exit policies and scoped wait deferral,
   verify human/failure bypass and evidence retention, then run the live comparison.

| Existing area | Implementation responsibility |
| --- | --- |
| `src/chat.rs`, `src/runtime.rs`, `src/usage.rs` | Optional versioned state, attempt candidates/receipts, trace reference and projection selection |
| `src/turn_context.rs`, `crates/context` | Typed contracts, validation, pure projection, evidence resolver adapter and tool host |
| `src/bots.rs`, `crates/bot/src/supervisor.rs` | Separate ingestion/processed watermarks, pending reasons, waits, continuation identity and budget |
| `src/watches.rs`, `src/watches/ops.rs`, `src/rooms/notifications.rs` | Delta ranges, wake policies, scoped staged acknowledgements and gaps |
| `src/worker.rs`, `src/claude.rs`, `src/history.rs`, `src/storage.rs` | Captured input, matching publication, handoff promotion, rollback and interrupted recovery |
| `src/tool_registry.rs`, `src/worker_tools.rs`, `crates/context/src/tool_policy.rs`, `crates/context/src/agent_policy.rs` | Shared discovery/dispatch, mutating-tool fence and managed guidance |
| `src/design.rs`, `src/delegation.rs`, `src/approvals.rs` | Existing paused-checkpoint and continuation chains retain handoff provenance |
| Chat/web/TUI views | Compact current disposition and trace access; retained lifecycle activity stays inspectable |

Use an explicit continuity schema version. During development, start with fresh
state rather than building a legacy-chat migration path. A future `control
migrate` is roadmap work. Do not wipe a running playtest or retrofit its attempts
and cursors. Start on isolated test chats for both providers; unknown versions
fail visibly. Ordinary chats and supervised bots may select different context
views, but they share the same append-only history and compactor/lens interface.
Enable the tested text-handoff/room-receipt contract for agent admissions;
bot approval controls unattended scheduling only. Do not advertise the broader
reference/watch-receipt contract until implemented.

## Acceptance tests

- **No-op churn:** replay sanitized join/leave telemetry and informational room
  posts under a committed wait. They remain inspectable but do not cause 17 model
  admissions; only the finite recheck or an actionable event can admit a turn.
- **Active consumption:** events arriving during a multi-action slice appear in
  context; committed receipts prevent duplicate wake admission. A racing newer
  event remains pending. Read-only delivery and failed saves consume nothing.
- **Paging and overflow:** multi-page reads, partial previews and spool gaps
  cannot skip unseen events. Explicit unavailable evidence reaches the model.
- **Atomicity:** inject failures before/after receipt save, workspace publication,
  journal append and final state save. Verify matching workspace/state/cursors,
  retained interrupted drafts, no duplicate successors, and other chats preserved.
- **Recovery and controls:** restart, cancellation, pause/resume, exhausted budget,
  queued human input, approval and blocking delegation preserve one writer and
  existing continuation-chain ownership. No automatic ambiguous-effect replay.
- **Authority:** reject foreign chat/resource references, forged ranges, stale
  generations, stale base revisions and conflicting idempotency keys. Model
  handoffs cannot replace instructions or admit knowledge.
- **Projection:** adding 100 unchanged wake cycles does not grow task context or
  evict standing instructions. Progress and failures remain visible. UTF-8 byte
  bounds, missing references and over-budget required inputs are explicit.
- **Provider parity:** the same committed state/deltas produce the same neutral
  envelope under Codex and Claude; adapter framing is captured separately within
  the required-framing guard. No provider-session ID is required for recovery.
- **Live comparison:** repeat equivalent tasks with explicit permission for
  multi-action slices. Record launches, useful actions per slice, monitoring-only
  turns, context bytes, cached/uncached input, output, model-slot time, elapsed
  task time, and progress against the goal. Compare protocol failures separately.

Run the relevant focused tests during each slice, then the workspace/web tests,
clippy, WASM and release builds required by CONTRIBUTING.md before publication.
Use isolated state roots for fault/live tests. Live provider checks are explicit,
not CI or doctor behavior. Report unperformed checks accurately in each PR.
The maintainer merges; deploy only when requested. Do not stop services, modify
the ongoing playtest, or clean its diagnostics as part of this implementation.

## Sol task brief

Implement the sequence above on feature branches from the current merged base.
Inspect overlapping request-panel work before touching chat presentation. First
deliver a reviewable projection/measurement slice with the regression fixture;
then the durable handoff/receipt contract, then active execution and selective
wakes. Keep each slice buildable and preserve saved-state compatibility. Do not
ship prompt-only changes as completion of the receipt/recovery contract, broaden
into a provider runtime rewrite, or claim semantic continuity from unit tests
alone. Publish PRs with validation and measured before/after evidence; do not merge.
