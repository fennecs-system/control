# Persistent swarm and self-improvement roadmap

Triaged 2026-11-05 against main `2d3492e` (#138). This is a plan, not a runtime
change. [The main roadmap](../roadmap.md) tracks shipped capabilities and release
gates. Stable IDs below identify independently reviewable increments.
Controller/ownership/budget review added 2026-11-06; main `42f8c42` (#139)
changes Cargo dependencies only and its Rust workflow passed. Proposals below
remain unimplemented.

## Direction and comparison

Control is a **swarm management platform**. The product ambition is an entire
engineering team of agents that can plan, implement, review, test and operate
systems under a person's direction. Agents specialize through lenses, personas,
tools and model choices; chats retain their work; rooms coordinate; services keep
running independently; admitted knowledge carries lessons between tasks.
Deterministic Rust supervision handles scheduling, ownership and budgets.

Lead with team coordination, inter-agent communication (A2A), computer use (CUA),
the steering/Rooms UI and self-improvement. Here A2A describes agent communication;
support for a particular external A2A protocol would need a separate adapter and
interoperability tests. CUA is a product workstream whose current capture tool and
planned interactive capabilities must be described accurately.

The steering-chat pattern worked: the operator redirected a lead and its workers,
and the team progressed from a house and renewable food to cobble walls, expanded
farming and diamonds. Preserve that interaction. Reliability and noise work should
improve productive sessions and quiet maintenance, with no compulsory redesign
of how every task is coordinated. The engineering-team ambition needs further
evidence on software delivery and operations; the playtest establishes useful
coordination, not complete replacement of all engineering responsibilities.

| Reference | Useful idea | Application to Control |
| --- | --- | --- |
| Hermes | Reusable procedural knowledge with progressive disclosure | Extend admitted claims into reviewed procedures, lens/persona revisions and tool proposals; measure reuse across agents. |
| Pi / Pi Durable | Small interfaces between models, storage, tools and execution; explicit recovery and background compaction | Separate Control's state transitions from effects; retain evidence and classify replay safety; use the existing continuation and context seams. |
| tinygrad | Small internal vocabulary, separate transformation and execution layers | Typed commands/events/effects with thin interpreters and measurable boundaries; tool implementations form a standard library. |
| Kubernetes | Desired/observed state, narrow reconciliation loops, ownership, admission quotas and explicit scheduling policies | Extend the existing supervisor with G1 conditions, O1 lifecycle ownership and Q1 team admission. Borrow contracts without adopting Kubernetes or a second control plane. |
| UniiChat | Hierarchical summaries with expansion to retained source text | C1 extends the existing comonadic history seams; summaries remain lossy derived views with source provenance. |
| Control playtest | Steering chat and communicating workers achieved successive goals, including diamonds; maintenance and recovery were noisy | Keep the working coordination pattern and make targeted admission, publication and visibility improvements. |

These projects supply useful ideas; Control's feature priorities come from the
needs of an agent team. Hermes also supports
[delegation](https://hermes-agent.nousresearch.com/docs/user-guide/features/delegation).
Its [skills](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills)
provide a useful procedural-memory reference, rather than evidence that Control
already learns reusable procedures.
[Pi Durable](https://earendil.com/posts/pi-durable/) separates storage and
execution and retains raw history during compaction. Its runtime/storage choices
are not dependencies of this plan. The
[tinygrad layers](https://docs.tinygrad.org/developer/developer/) are an analogy
for separation, not an agent scheduler or a required inference backend.
Control remains Rust/WASM/Nix with plain files and atomic commits. Upstream
non-Rust runtimes stay in explicitly configured tool/agent containers.

[UniiChat](https://github.com/VictorTaelin/UniiChat) is Victor Taelin's project,
already linked in the main roadmap; unrelated products named UChat/UniChat are
not the intended comparison. Pi Durable also describes durable timers and
dependency waits, so "no scheduling" would mischaracterize that runtime.
Summary trees support bounded context and source expansion; they do not by
themselves replace full-text search or guarantee recall of every fact.

The [Kubernetes controller model](https://kubernetes.io/docs/concepts/architecture/controller/)
is a useful organizing analogy. Control already has pure supervisor decisions,
source cursors, coalescing, waits, generations and per-bot turn budgets;
[the bot-control research contract](research/BOT-CONTROL.md) also proposes desired
state and typed effects. The missing increment is a shared, durable account of
intent, observed conditions and team admission, rather than inventing supervision
from scratch. Kubernetes resource quotas are not an implementation of model-spend
limits, and a controller cannot infer completion of an arbitrary prose goal.

## Evidence and reconciliation

Inputs: `fix.tmp` (local ignored planning inventory, preserved), the uncommitted
`docs/LONG-RUN-CONTINUITY.md` draft (preserved),
[turn continuity](TURN-CONTINUITY.md), [architecture](ARCHITECTURE.md),
[crate boundaries](CRATE-BOUNDARIES.md), [knowledge](KNOWLEDGE-TOOLS.md),
[delegation](DELEGATION.md) and lead chat `c-18e4b7cadfa33158-11` with its Alice/Cass
children. Private chat content and workload scripts are not copied into this plan.

The lead chat was read during this review. It contains repeated maintenance
responses to room/watch/timer wakes, later steering for cobble walls, diamonds
and farm expansion, and a final `/cancel all` that stopped bots while retaining
services. Those reports support the coordination diagnosis; prose alone does
not independently verify the world state. The draft's deeper trace measurements
below are attributed to that earlier analysis, not recomputed from the bounded
activity API in this review.

| Evidence | Triage decision |
| --- | --- |
| Draft: during 03:25–04:09 UTC maintenance, Alice started 55 attempts; starting prompt stayed approximately 14 KB. Her own callback-status output caused a wake. | R2 reduces unnecessary admissions; C1 reduces repeated context cost on useful turns. Typed projections and lazy evidence reads also reduce input size. |
| Draft: three redirected Alice attempts hit the 600-second deadline; a staged handoff did not become committed state. | R1: reserve finish time and expose uncommitted recovery evidence; do not replay uncertain effects. |
| Draft: lead made 36 attempts in about 39 minutes after redirection; 13.59M recorded input tokens, 1.20M excluding cache hits. | Tighten lead prompting and coordination. These are cumulative reported inputs, not prompt size or a billing estimate; failed-attempt usage is incomplete. |
| Lead record: repeated no-change maintenance replies and occasional requests to verify footing. | Separate changed facts and actionable failures from routine audits; preserve real alerts and human steering. |
| Draft: inventory loss, movement/crafting failures and surprising placement responses. | Keep a separate workload/protocol investigation; no confirmed general harness root cause or game-specific core fix. |
| Current source: `history::activity` defaults failed reads to empty; HTTP accept spawns a handler thread per request. | R3/R4 remain concrete reliability work. Model slot limits do not bound HTTP threads. |
| Current source: Lens selection uses `NoSummaries`; the server still owns broad mutable `State`. | C1 and A1–A4 remain real gaps despite journal/core-crate progress. |

Do not reimplement shipped P10/P11b/P12/P14/P16, journal/outbox work, ancestor
request visibility or #134 continuity composition. The historical inventory's
“open”, “branch” and prerequisite labels are not authoritative today. In
particular, bot callbacks already exist without completing the proposed off-lock
writer: that writer is a coupling/performance improvement, not a prerequisite
for introducing a second supervisor. Historical restrictions such as “never
touch control-core” applied to those slices; current domain placement follows
the documented crate contracts.

## P0: continuity and observable failures

| ID | Small implementation slice | Acceptance |
| --- | --- | --- |
| R1a | Derived active/finishing/ended phase, with an initial measured finish reserve of 60 seconds within the existing 600-second limit. Advertise it on ordinary responses without needing a callback. Fence new host workload mutations while allowing bounded status, cancellation and handoff. | Fake-clock boundaries, no new mutating host work in finishing, successful publication for both CLI providers; ignored wind-down still times out normally. Native CLI private tools and independently running services are outside this fence. |
| R1b | Retain failed generation, last committed checkpoint, termination cause and draft/log references as explicitly uncommitted recovery evidence. Present it at the next authorized turn. | Failed publication leaves committed notes/cursors intact; missing drafts visible; reconciliation markers commit with the next matching checkpoint. Cancel/pause does not auto-retry; ambiguous effects are inspected, never blindly replayed. |
| R2 | Select relevant events and reconcile known intent/observations before admitting a turn. Retain full watch output; exit-only or bounded literal-prefix-plus-exit policies provide an incremental filter for untyped streams. Preserve provenance, source sequences and eligible references. | Unchanged status-query/own-output telemetry causes zero admissions. Human steering, relevant child/tool completion, failure and source loss remain visible. Restart, stale observations, coalescing and missing spool tested. Reading is not acknowledgement. |
| R3 | Distinguish missing/corrupt history from an empty activity record in UI and model context. | Missing/decode-failed records preserve cause and references; ordinary empty records remain empty. |
| R4 | Bound HTTP handler admission and return a predictable overload response; retain request deadlines. | Slow-client saturation and recovery stay within the bound; worker progress and shutdown remain responsive. |

R1a/R1b form the recovery increment; R2 is independently testable. R3 and R4 can
ship as isolated fixes. Keep typed delivery-retirement bookkeeping out of task
context while retaining lifecycle records. Continue the existing shared agent
contract: a bot adds approved scheduling, not a different memory/runtime system.

R2 treats a wake as a hint to run a deterministic decision, not an instruction to
call a model. A known satisfied condition with no pending work produces no turn.
An opaque goal such as "reach diamonds" requires verified observations or an
attributed agent checkpoint; the host must not pretend to interpret arbitrary
logs. Unknown/stale state is not success. Preserve bounded rechecks and source-gap
diagnostics. Suppress own room echoes and unchanged diagnostic output, while
retaining relevant completion/failure events from the bot's own tools. Origin
alone does not establish irrelevance. G1 adds structured conditions to this
increment; R2's immediate filtering fix need not wait for the entire schema.

R1's bounded finishing phase resembles a cleanup boundary, but is not literally
a [Kubernetes finalizer](https://kubernetes.io/docs/concepts/overview/working-with-objects/finalizers/):
finalizers block deletion until cleanup completes and do not supply an automatic
grace period. Keep attempt wind-down, durable publication and resource cleanup
separate; failed cleanup stays visible without holding a model slot indefinitely.

Change workload guidance separately: act while useful work remains, check deltas
at meaningful boundaries, publish before expiry, send changed facts/milestones/
blockers, and wait after completion. A lead intervenes for changed goals,
dependencies, evidence or stalled progress. Routine audits should not repeatedly
nudge a productive builder. Advertise finish-phase instructions only after R1a.

## P1 foundation: intent, ownership and team admission

Deliver these as narrow extensions of current decisions and durable effects.
They precede the corresponding E1/W1/U1 features, not the independent P0 fixes
or the entire A1–A4 extraction.

**G1: desired state and observed status.** Extend `control-bot` with a versioned
definition/intent and separately projected status. Capture goal reference,
definition generation, owner/team, lifecycle intent and permitted triggers.
Status records observed generation, active attempt, source positions and typed
conditions such as Progressing, WaitingDependency, NeedsDecision and Degraded.
Each condition has true/false/unknown state, reason, evidence/request references
and last transition; observations carry freshness and provenance. Model-reported
progress remains attributed, distinct from host-verified resource state.

Reconcile from admitted events, policy and time without a model call. Emit work
only for an unsatisfied known condition or relevant pending input; record why a
turn was admitted or suppressed. Persist intent/decision before effects, reject
stale generations and retain existing receipts, bounded rechecks and one writer
per chat. U1 renders this projection in the existing UI. Acceptance: duplicate
wakes produce no duplicate effects; a satisfied idle bot stays quiet; unknown
or stale evidence remains visible; changed steering and real failures get through.

**O1: lifecycle ownership.** Record lifecycle owner, borrowing relationships and
explicit retain/cascade cleanup policy for children and services. Chat parentage
is coordination lineage, not proof of resource ownership. On cancellation or
owner deletion, reconcile the policy against actual resources; retain shared or
borrowed services and diagnostic references. Adoption/reparenting requires
authority and a captured revision. Attempt completion alone must not stop an
independently owned persistent service. Acceptance: cancelled lead, borrowed
service, cleanup failure and restart yield the same visible ownership outcome.
Use the [owner/dependent model](https://kubernetes.io/docs/concepts/architecture/garbage-collection/)
as a reference while preserving Control's explicit resource lifetimes.

**Q1: team quotas and admission priority.** Add durable team/bot reservations to
the existing slot, queue and turn limits. Count child work, retries, review and
summary jobs against their owning team; spawning a child must not reset limits.
Atomically reserve before admission and reconcile usage/release by stable attempt
identity. Unknown outcomes retain a visible reservation until reconciled.
Expose limits, remaining capacity, queued reasons and observed/unknown usage.

Start with enforceable turn/concurrency limits and conservative model-call
reservations where the adapter exposes bounds. Delayed CLI usage telemetry
supports accounting and future admission limits, not a strict in-flight token or
currency ceiling. Price revisions, estimates and missing usage stay explicit;
local inference also consumes bounded hardware capacity. Borrow aggregate
admission from [ResourceQuota](https://kubernetes.io/docs/concepts/policy/resource-quotas/),
without claiming Kubernetes itself solves model-spend enforcement.

Use non-preempting priority classes: interactive/decision work, normal workers,
then background summaries, with fairness/aging and bounded queues. Priority does
not evict an active provider or create capacity; test parent/child resolution
with one slot and yield genuinely blocked work. Any reserved decision capacity
must be explicit. Never bypass team grants, budgets or the single-writer rule.
Acceptance: concurrent admissions cannot oversubscribe reservations; restart,
cancellation and partial usage do not double-charge or silently reset budgets;
background jobs neither starve steering nor starve indefinitely under the policy.

| Placement | Responsibility |
| --- | --- |
| Pure `control-bot` / domain core | Intent/status transitions, conditions, ownership references, budget/admission decisions and typed effects |
| Application/persistence/effect adapters | Durable reservations and receipts, authorization, clocks, model calls, resource inspection and cleanup |
| Standard library and versioned configuration | W1 schedule interpretation, W2 recipes, L1 persona/SOUL bundles and default compaction strategy; compile to the same commands/effects |
| Existing context/model extension seams | C1 summary storage and pure cuts; hosted/local summary workers through M1; C2 small user controls |

Product features can ship together without becoming new concepts inside the
supervisor. Reuse existing `Compactor`, `Summaries` and Lens seams rather than
introducing a second parallel plugin system or putting network calls in pure code.

## P1: upward escalation and prompting

E1 complements the existing steering chat, room messages and direct follow-ups.
Agents continue routine decisions within their role. Use a tracked request when
a blocker needs an answer and durable resumption; ordinary collaboration does
not require a request form or a formal workflow.

E1 adds a durable worker-to-lead request and scoped resolution path alongside
existing human design questions. Today descendant questions can appear in the
ancestor UI; `control_agent_message` sends downward follow-ups. Neither supplies
a structured child-to-lead decision/resumption contract.

| Request | Resolver | Result |
| --- | --- | --- |
| Help, owned blocker, task tradeoff within delegated scope | Assigned lead | Evidence-linked answer or narrower task direction |
| Goal/scope change beyond the lead's mandate, or unresolved human preference | Operator through the existing question UI/Discord path | Attributed direction for the same request |
| Additional access, resource or capability | Existing permission broker/operator | Separate scoped grant or denial; a lead's answer is never a grant |
| Reusable lesson or procedure revision | Knowledge/procedure review | Candidate revision with evidence, evaluated and admitted separately |

Persist request ID/type, originating actor/chat/run/generation, parent lineage,
question, evidence references, proposed options, resolver, status, resolution
and continuation reference. Reuse durable pause/checkpoint and resume machinery.
Only the blocked dependency pauses; unrelated swarm work can continue. Deduplicate
requests by identity rather than fuzzy prose; a repeated blocker updates the
same request instead of spawning more repair children.

Project a pending request into G1's NeedsDecision/blocked status with its request
reference. The lead reconciles a changed child condition through existing durable
delivery; U1 shows the same condition. The condition does not replace the request's
answer, authority and resumption protocol. Avoid a new message bus and repeated
prose notices for an unchanged blocker.

A parent waiting on its child must be able to receive and resolve that child's
request. Test the one-model-slot case, a full queue, restart, duplicate/stale
answers, reparented or cancelled work, authority revocation and answer-save
failure. Resolution must neither create a second writer nor deadlock behind the
child it must unblock. If the lead cannot resolve or is unavailable, forward
the same request visibly to the operator under a bounded policy.

E1's prompt review spans tool descriptions, ordinary/spawned agents, bot wakes,
judges and continuation prompts. Replace the blanket “take your recommended
option” instruction with a scope-aware rule: decide routine authorized steps;
ask the lead when a blocker exceeds the worker's remit; ask the operator when mandate or preference
is missing; use the permission path for access. Waiting for a healthy peer is
not itself an escalation. Check scenario fixtures across those entry points;
prompt text never substitutes for deterministic policy tests.

## U1/U2: coordination UI, A2A and computer use

**U1: retain the steering chat as the team's control surface.** Keep the current
Rooms layout and theme: room selection, broad conversation area, participants
and useful links. Make ownership, current objective, progress, blocked dependency
and the latest changed fact easy to inspect. Collapse routine activity while
retaining logs and evidence. Keep steering, pause/cancel and actionable questions
close to the work; show whether a message was queued, delivered or processed
where the existing receipt contract can establish it. Avoid a new dashboard
that forces users to abandon the useful chat flow.

Use G1 conditions and O1 ownership as the data model, including observation age
and last transition. Show unknown progress honestly; do not derive authoritative
status by parsing the most recent assistant sentence.

A2A work builds on rooms, child results and directed messages. Preserve attribution,
lineage, evidence links and explicit destinations. E1 fills the decision/resumption
gap. First improve the user's ability to see and direct this communication; remote
protocol interoperability is separately scoped. Acceptance: steer an active team,
follow a handoff, find one real blocker amid retained telemetry, and resume without
duplicate tasks or missed human direction.

**U2: make computer use a visible product capability.** Start from offline browser
capture and the [browser demonstration](DEMONSTRATIONS.md), then add scoped
interactive browser observation and actions with visible evidence, persistent
session handles and human handoff. Wider desktop control can follow a demonstrated
need. Existing capture is not yet an interactive browser or desktop CUA runtime.
Reuse resource, transport, tool and policy contracts; do not wait for all A1–A4
extractions. Acceptance: an agent builds a page, another exercises it in a browser,
reports a reproducible defect, and verifies the fix in the steering conversation.

## W1/W2: schedules and a small workflow layer

Make recurring team work easy to configure. Start with **W1 cron-style schedules**
over the existing supervisor/timer machinery: choose a team or chat, goal, schedule
and time zone, then inspect the next run, recent outcomes, pause/resume and run-now.
Capture the definition revision and a stable scheduled-occurrence ID so restart
does not submit the same occurrence twice. Existing model slots, bot approval,
budgets and per-chat ordering still govern admission.

Borrow explicit [CronJob policies](https://kubernetes.io/docs/concepts/workloads/controllers/cron-jobs/):
Allow, Forbid (default), or Replace, plus a missed-run start deadline. Concurrency
policy applies per schedule; Q1 still bounds the team. Allow creates independently
owned instances and never concurrent writers in one chat. Replace requires
confirmed cancellation/quiescence of the prior writer and reconciliation of its
effects; defer enabling it until that contract is tested. Show skipped occurrences
and reasons. Initially choose at most the latest eligible missed occurrence within
the deadline; document this Control catch-up policy rather than claiming complete
Kubernetes equivalence. Specify daylight-saving gaps/repeats and test previews,
restart, clock changes, edits and pause racing a due trigger. A deduplicated trigger
does not make uncertain external effects safe to replay. Schedule ticks never
restart a progressing provider attempt; a later admitted attempt starts fresh.

By default scheduled runs and their descendants cannot create, edit or enable
schedules. Enforce origin/capability in the host so delegation cannot bypass it;
any operator-authorized exception is scoped and auditable. Hermes has a similar
[default guard](https://hermes-agent.nousresearch.com/docs/user-guide/features/cron),
with an opt-in override; it is not an unconditional upstream prohibition.

**W2 is a small declarative recipe layer** for repeated team patterns: triggers,
roles, named steps, dependencies, bounded parallel branches, completion checks,
failure handling and result destinations. Reuse existing spawn/message/wait/tool
operations and typed supervisor effects. A simple form and versioned configuration
can expose the same definition; defer choosing a new custom syntax until real
recipes establish what people need. No authored JavaScript/Python integration.

Example recipe, illustrative rather than supported syntax:

```text
Trigger: weekdays at 09:00 in the team's time zone, or run now
Lead: select one issue from the approved project scope
Builder: implement in an isolated workspace
Reviewer + tester: review the diff and run checks in parallel
On failure: return evidence to the builder, within the retry budget
On success: prepare a draft PR and post one result to steering chat
On unresolved blocker: request a decision; resume this step after the answer
```

Persist instance/step identities, inputs, results and wait reasons with the
existing intent/receipt rules. Capture approved definition revisions for active
instances; edits affect future instances unless explicitly applied at a safe
boundary. Retry only classified safe operations or reconcile uncertain outcomes.
Users can inspect, steer, pause and cancel through the same chat/UI. Workflow
definitions describe work within existing grants; they do not grant access.

Validate recipes before activation: schema and referenced revisions, dependency
cycles, fan-out/retry limits, required capabilities, budgets, ownership and result
destinations. Require a bounded dry-run/fixture trace for the selected revision;
show failures before enabling triggers. A changed recipe needs renewed validation.
Implement interpretation as standard-library operations over G1/O1/Q1, not a
workflow-specific state machine embedded in the supervisor.

First acceptance: a scheduled inspect/build/review recipe survives a restart and
a human decision wait, posts one outcome and leaves unrelated work running.
Ad hoc teams continue to work without recipes. Keep generalized workflow languages,
distributed scheduling and arbitrary executable hooks outside this first layer.

## Supporting work: decouple state and execution incrementally

Use A1–A4 to remove coupling encountered while delivering the product work above.
The full extraction is not a prerequisite for improving the successful team flow,
adding schedules or demonstrating learning.

Target dependency direction:

```text
web / TUI / Discord / MCP
          ↓ typed commands, bounded queries
application operations + authorization + durable acceptance
          ↓
pure domain transitions → stamped effects
                              ↓
             persistence / tools / engine / model adapters
```

| ID | Work | Exit gate |
| --- | --- | --- |
| A1 | Unify command/approval/continuation operations across transports; handlers take narrow typed inputs and projections. Group state by domain with borrowed views before changing lock ownership. | Same authorization, lineage, rollback and checkpoint outcomes from each transport; no new public `Arc<Mutex<State>>` dependency. |
| A2 | Move expensive rendering/reads and selected persistence work outside the broad lock. For writes, explicitly distinguish pending intent from durable completion and revalidate generation/authority before effects. | Concurrent save/failure/cancel tests, no rollback over newer changes, durable-before-effect, visible degraded persistence; benchmark under concurrent fsync load. Migrate callers incrementally. |
| A3 | Lazy bounded transcript/activity reads and explicit checkpoint projections; runtime-owned journal readers, bounded caches and visible source gaps. | Startup and resident memory scale with working set, not full transcript hydration; stable source positions, restart and disk-full tests. JSON/JSONL plus atomic file manifests; preserve cross-domain commit atomicity. |
| A4 | Finish narrow Host/ToolSet operations and the common execution Engine per area; built-in tools form a standard library. Extract web/TUI/MCP crates only once their application interface is real. | Dispatch parity including denials, cancellation and ownership; live engine tests per migrated area; removal of legacy full-state access measured per slice. |

The existing `control-core`, `control-effect`, `control-bot`, `control-ai`,
comonad and context crates are the starting point. No mandatory async rewrite,
global event bus, second tool catalogue or crate-per-file split. Domain locks
come only after narrow borrows and measured contention, with a fixed order.
Storage-layout changes preserve durability and require fault tests; plain-file
storage is the chosen direction. Architectural work should yield usable slices.

A4 enables operator-registered container-backed tools through the same catalogue.
Registration/discovery does not authorize execution. Schema/version checks,
namespacing, duplicate detection and scoped resource grants precede activation.
Task-specific game adapters remain in their granted workload containers.

## P2: model roles and local inference

M1 defines separate captured selections for worker, lead, reviewer and compactor.
Start with a bounded model request/result/usage interface usable by the summary
worker, then add native API and configured local inference through Rust adapters.
CLI providers continue to work. A provider choice does not change tools,
permissions, workspace publication or the supervisor.

Every new attempt is a fresh provider session reconstructed from Control's
committed journal, standing instructions and selected context. Within an active
attempt the provider may keep its session/KV state and perform multiple steps;
callbacks do not force a restart. Once it ends, recovery and continuation create
a fresh attempt, never resume its provider session. Provider caches are optional
optimizations, never the authority for task state. This applies equally to CLI,
native API and local adapters.

Record provider/model revision, parameters, context/output limits, capabilities,
usage semantics and opaque credential reference in execution provenance. Verify
availability and fail visibly; listing a model does not establish access. Local
endpoints declare supported streaming, tools and structured output instead of
assuming API compatibility. Pin model/runtime/quantization identity where
available. Bound concurrency, memory and context; report missing usage honestly.

Use Nix-declared isolated inference services where supported. Benchmark available
hardware, latency, memory and task quality before choosing a runtime; tinygrad
is an optional experiment, not a migration prerequisite. Do not introduce a host
Python/Node adapter or treat local inference as zero-cost. Tool-capable local
agents must pass cancellation, malformed-output, authority and recovery contract
tests. A text-only summarizer can ship sooner, with no tool execution rights.

## C1–C3: infinite chat and compaction model selection

“Infinite chat” means a continuous conversation with bounded active context over
growing retained history. Disk retention, artifact budgets and explicit gaps are
still necessary. Compaction is a derived view, never deletion or automatic
knowledge admission. Keep append-only positions and the existing Store/zipper,
`Summaries`, Lens cut and zoom seams; preserve Recent/Passthrough options.

- **C1: summary storage and worker.** Immutable source blocks acquire leaf and
  pairwise merged summaries in a lazy tree. Background effects call the selected
  model; pure code selects a budgeted cut. Publish complete validated nodes
  atomically at a turn boundary. Deduplicate work and retain the last usable view
  on failure. The worker needs A3's bounded reads and M1's summary-call path,
  not a complete provider/runtime rewrite.
- **C2: small user controls.** Expose automatic compaction on/off and an independent
  compaction model selector: configured hosted, local, or an available same-model
  adapter. Keep history strategy, reserves, output bounds and resource limits in
  versioned node defaults/advanced configuration under Q1, with permitted per-chat
  overrides. Implement pluggable strategies through existing context seams, with
  hosted/local workers as effect adapters. Disabling automatic summaries stops
  new jobs; it does not erase summaries or raw history. Retain bounded deterministic
  selection and show omissions. No seven-setting panel is required to use a chat.
- **C3: inspect and expand.** Show selected model and revision, source ranges,
  summary age/status, usage/cost where known, and omitted/unavailable evidence.
  A bounded zoom/read tool expands by stable reference down to original text.
  Pin standing human constraints, unresolved requests, committed working state
  and recovery warnings outside lossy summaries, within the whole input budget.

The compaction model is independent of the conversation model. For the requested
OpenAI 5.6 example, evaluate
[`gpt-5.6-luna`](https://developers.openai.com/api/docs/models/gpt-5.6-luna), which
official documentation describes as cost-sensitive. It is a candidate, not a
hard-coded cheapest/default promise: choose using current account availability,
measured recall and total cost. Local-model selection is equally explicit.
[Pi's custom compaction](https://pi.dev/docs/latest/compaction) also demonstrates
using a separate summarizer; Control implements its own typed boundary.

Cache identity includes access scope, immutable source IDs/hashes and ranges,
child-summary hashes, model/provider revision, prompt/schema version and relevant
generation settings. Capture that configuration per summary job. Switching models
creates a new cache namespace; old nodes retain provenance. Do not rewrite raw
history or rebuild every historical node immediately. Prevent mixed-revision
publication and cross-chat/actor evidence leakage.

If a model is offline, slow, unaffordable or returns malformed output, retain the
prior view and show the failure. A preconfigured permitted fallback may run;
never silently send local-only history to a cloud provider. Switching compaction
off does not turn a bounded model into an unlimited one. If mandatory constraints
and selected evidence cannot fit, surface a context-budget condition rather than
silently dropping them. Schedule summary jobs below interactive work with separate
bounded queues/budgets and cancellation; no unbounded background model loop.

This selector controls **Control's summary service**. It does not promise to
replace a CLI provider's private auto-compaction model. Configure the projection
sent to that provider and report those two layers distinctly. A configured native
API/local summarizer is usable even while the chat runs through a CLI adapter.

Acceptance: large synthetic history with bounded memory/context; lookup of old
constraints and evidence after multiple merges; unresolved actions and late
steering retained; corrupted/missing source and summary nodes; model switch,
restart, stale completion, cancellation, queue exhaustion and local outage.
Evaluate hosted/local candidates on identical recall and contradiction fixtures,
including prompt injection embedded in old tool output. Summaries preserve source
trust labels and cannot turn quoted text into policy. Measure fidelity, latency,
total summarization plus conversation cost and provider cache hits separately.
“Cheaper per token” alone is not success.

## L1: versioned lenses, personas and SOUL.md

Keep four distinct things: a lens selects context/tool descriptions; a persona
describes behavior and role; a procedure explains how to do a class of work;
policy grants enforceable capabilities. Complete lens-based execution filtering
through the tool host where intended; a descriptor or persona cannot grant access.

Add versioned persona bundles referencing composable lenses, optional `SOUL.md`
prose, procedure revisions, model-role preferences and proposed resource needs.
These are standard-library/configuration artifacts resolved at admission; the
supervisor does not interpret persona prose or acquire a persona-specific lifecycle.
`SOUL.md` supplies persistent identity, tone and collaboration preferences. It is
inert, reviewed content with explicit ownership and scope, not a mutable secret
system prompt or a permission file. Personal definitions live under configured
home roots; shared examples must contain no private configuration.

Define deterministic composition and display effective context: enforced node
policy; current authorized task/steering and standing constraints; selected
persona/soul; relevant procedures/knowledge; attributed evidence. Conflicts stay
visible. Stale persona prose cannot override newer user direction; tool/page
content cannot promote itself to instruction. Capture resolved revisions per
attempt, apply accepted changes at a boundary and retain rollback. Children get
explicit role/identity bindings; do not copy every parent's identity or entire
history by default.

Acceptance: stable composition and source traces, bounded progressive disclosure,
conflicting lens/persona inputs, revision changes during a run, rejection of
capability escalation, independent sibling identities and restart/reuse.

## P2: a reviewed self-improvement loop

Separate working notes, factual knowledge, reusable procedures and source changes.
All can inform improvement, but they have different admission and rollback paths.
Model-weight training is a later research track, not required for this loop.

```text
task → action → evidence → verification → lesson proposal
                                         ↓
                              review / evaluate / admit
                                         ↓
                        versioned knowledge / procedure / lens
                                         ↓
                              reuse on a subsequent task
```

**L2: procedural learning.** Extend the existing knowledge proposal/review pattern
with typed procedure/lens/persona candidates: motivation, applicability,
preconditions, steps, failure modes, counterexamples, source attempts and test
evidence. Use deduplication, revision/supersession, expiry/recheck and rollback.
Reviewers receive bounded evidence and can disagree. No admission solely because
an agent declares success. Retrieval loads relevant accepted revisions, not every
lesson into every prompt. Preserve individual and shared scope.

First demonstration: two agents encounter the same generic setup/verification
problem. One proposes a procedure after deterministic verification; review admits
it; a fresh agent retrieves it, succeeds with fewer redundant actions, and records
the revision used. Also test a stale/revoked procedure and a misleading proposal.
Measure failure recurrence, useful reuse, review burden, task cost and regression
rate against a baseline. Minecraft protocol details stay in workload knowledge.

**L3: source and tool self-development.** Use existing architect-authorized
improvement workspaces, offline builds, child review and GitHub draft publication.
Capture a reproducer and baseline, propose a bounded patch, run deterministic
checks, obtain review evidence, publish a reviewable change, and retain provenance.
Tool/persona changes follow their respective admission paths. Merge/deploy remains
under existing maintainer/operator authority; neither a judge nor a learned skill
can expand it. Add selectable reviewers with M1 availability checks before claiming
independent multi-model review. Runtime registration never auto-installs agent code.

## Regression lanes, deferrals and delivery evidence

Map the old inventory into current work rather than copying its whole PR sequence:

| Existing area | Disposition |
| --- | --- |
| R3/R4 in `fix.tmp` | R3/R4 above; reproduced at source boundaries in this review. |
| PR 3, 4/4b, 13a–c, engine E1–E5 | A1–A4; audit already extracted operations per slice; measurements gate lock/storage changes. |
| P11/P11b and buried blockers | E1; ancestor UI and routine-step guidance already exist; upward resolution is the gap. |
| P17 and continuity follow-ups | R1/R2 plus workload guidance and existing #134 regressions. |
| S1/S2, B6 and retained instruction growth | A3 and C1–C3; journals shipped, computed summaries remain open. |
| B5/PR 11 and self-improvement ideas | L1–L3; reuse approved bot identities, knowledge and source-improvement tools. |
| Child-result lineage, typed activity, room discovery/focus and historical approval hypotheses | Reproduce against main before reopening; preserve existing tests and scope boundaries. |
| K1 knowledge graph rescans, transcript invalidation and remaining polling | Follow measured hot paths after A1; independent small fixes when evidence justifies them. |
| Disk quotas, launch-to-record crash window, aggregate resource/spend budgets | Q1 makes team admission explicit before higher fan-out/unattended scale; disk quotas and launch recording retain their separate reliability gates. |
| V2 ingress credit / relay aggregate memory | Measure separately; #138 FIN-drain fix is merged and requires rebuilt images when deployed. |

U1/U2 and W1/W2 make coordination, computer use and recurring team work explicit
product increments. Keep remote execution/federation, inference bundles,
general-purpose workflow languages and weight training behind demonstrated local need. Local
models through a bounded adapter are P2; bundling every runtime/hardware target is
later. Preserve plain-file storage and explicit migrations, not automatic wipes.

Each implementation PR states its ID, observed problem, contract changed,
dependencies, checks and remaining limits. Run the relevant
[contribution checks](../CONTRIBUTING.md); engine changes additionally need live
resource tests. Fault/cancellation tests precede claims of recovery correctness.
Planning-only changes require document/link/diff checks, not model calls.

For R1/R2, follow the continuity draft's controlled test: fresh isolated worlds,
rooms and chats with seed `-6834438708894114399`, pinned revisions/models/resources
and identical objectives. Preserve the existing playtest. Compare deadline and
wake changes separately before combining; label a combined-only run exploratory.
Observe 30 minutes of maintenance after completion, then the same redirected work.
Record milestones, useful actions, admission causes, deadline/publication failures,
committed-state age, context bytes, reported usage and missing usage. Report sample
size and response to real failures/new steering, not just reduced wake count.

The first implementation batch is targeted R1a/R1b and R2 changes, with R3/R4 small
reliability fixes. G1 supplies conditions before E1/W1/U1 consume them; O1 defines
their resource lifetimes, and Q1 gates increased unattended fan-out and scheduled
model work. U1/W1 improve the visible team experience; E1 fills a specific
coordination gap. Apply narrow A1–A4 extractions where those changes need them.
U2, W2 and the first L2/L3 engineering demonstration can proceed independently of
the full architecture/model/compaction program. C1 can begin with pure cache/cut
fixtures while M1 is built. Measure delivery quality, useful coordination, human
interventions, noise and reuse. No implementation, new model spend or deployment
is performed by this roadmap update.
