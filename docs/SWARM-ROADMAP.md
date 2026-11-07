# Persistent swarm and self-improvement roadmap

Triaged 2026-11-05 against main `2d3492e` (#138). This is a plan, not a runtime
change. [The main roadmap](../roadmap.md) tracks shipped capabilities and release
gates. Stable IDs below identify independently reviewable increments.
Architecture and dependency review updated 2026-11-07. Main `42f8c42` (#139)
has a passing Rust workflow. Proposals below remain unimplemented.

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

Preserve the successful steering-chat pattern: the playtest team responded to
redirection and reported progress through building, farming and diamonds.
Engineering-team replacement remains an ambition to validate on delivery and
operations tasks. Improve the working flow through measurable reliability fixes.

**Pi/tinygrad layering is the architectural constraint.** Keep a small pure
vocabulary for lifecycle, generic admission/waits and typed effects. Implement
product behavior as tools, strategies and configuration over narrow application
operations; adapters own I/O. A new feature does not justify a new core concept.
Every implementation PR must identify its layer and explain any core API change:
which generic invariant cannot be expressed through the existing seam? Merely
being useful to several tools is not sufficient reason to move their behavior
into `control-core` or `control-bot`. Tool implementations still use host-enforced
authorization and durable acceptance; a prompt cannot enforce those invariants.

| Reference | Useful idea | Application to Control |
| --- | --- | --- |
| Hermes | Reusable procedural knowledge with progressive disclosure | Extend admitted claims into reviewed procedures, lens/persona revisions and tool proposals; measure reuse across agents. |
| Pi / Pi Durable | Small interfaces between models, storage, tools and execution; explicit recovery and background compaction | Separate Control's state transitions from effects; retain evidence and classify replay safety; use the existing continuation and context seams. |
| tinygrad | Small internal vocabulary, separate transformation and execution layers | Typed commands/events/effects with thin interpreters and measurable boundaries; tool implementations form a standard library. |
| Kubernetes | Desired/observed state, ownership and explicit scheduling/admission policies | Borrow contracts at their existing boundaries; G1 projects status, O1 audits resource lifetimes and Q1 aggregates admission. No Kubernetes-shaped core object model. |
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

[UniiChat](https://github.com/VictorTaelin/UniiChat) is Victor Taelin's project.
Its summary trees support bounded context and source expansion, not automatic
full-text search or guaranteed recall. Pi Durable also supplies durable timers
and dependency waits.

The [Kubernetes controller model](https://kubernetes.io/docs/concepts/architecture/controller/)
is a useful organizing analogy. Control already has pure supervisor decisions,
source cursors, coalescing, waits, generations and per-bot turn budgets;
[the bot-control research contract](research/BOT-CONTROL.md) also proposes desired
state and typed effects. A controller cannot infer completion of an arbitrary
prose goal. Resource quotas are not model-spend enforcement.

## Delivery order

This is the canonical priority/dependency table; `roadmap.md` links here.
G1/O1/Q1 are consumer-scoped changes, not a foundation release ahead of all P1.

| Priority | Work | Minimum dependency / first exit |
| --- | --- | --- |
| P0 | R1a/R1b finishing and recovery; R2 watch admission; R3 history errors; R4 HTTP limits | Independent fixes with fault tests; preserve useful progress and real alerts. |
| P1 | E1 escalation with G1 request/dependency projections | Existing lifecycle and durable requests; one-slot resolution works. No broad spec/status subsystem. |
| P1 | U1 coordination UI and A2A visibility | Read existing lifecycle, requests and ownership; add projections as needed. No dependency on all G1/O1/Q1 work. |
| P1 | W1 bounded schedules | Existing timers/admission, durable occurrence IDs and shared schedule-write authorization. Q1 before expanding team-wide fan-out; Replace deferred. |
| Supporting | Q1 team admission; O1 ownership audit/fixes | Aggregate existing counters; extend ownership only for a demonstrated gap or a consumer changing resource lifetimes. |
| Supporting | A1–A4 state/effect boundaries | Establish the narrow operation a feature needs as part of that slice. No full extraction gate. |
| P2 | U2 CUA; W2 workflow tools | Scoped browser/resource operations; validated recipes using existing spawn/wait/message primitives. |
| P2 | M1 model adapters; C1–C3 summary strategies | Bounded reads and summary-call adapter enable C1 before general local tool execution; on/off plus model selector. |
| P2 | L1 lenses/persona/SOUL configuration; L2/L3 reviewed improvement | Captured revisions, scoped tools, evaluation and admission; no new supervisor personality or learning lifecycle. |
| Later | Federation, inference bundles, general workflow languages, weight training | Demonstrated local need. |

## Evidence and reconciliation

Public contracts: [turn continuity](TURN-CONTINUITY.md),
[architecture](ARCHITECTURE.md), [crate boundaries](CRATE-BOUNDARIES.md),
[knowledge](KNOWLEDGE-TOOLS.md) and [delegation](DELEGATION.md).
The operator's private playtest and continuity analysis informed triage; their
relevant observations are summarized below. They are not reproducible public
benchmarks or implementation prerequisites. Preserve the original local records.

| Evidence | Triage decision |
| --- | --- |
| Private analysis reported 55 worker attempts during 44 minutes of maintenance, repeated roughly 14 KB starting prompts and status-output wakes. | R2 targets redundant admission; C1 and bounded projections target context cost per useful turn. Reproduce with a public synthetic watch fixture before attributing a specific cause. |
| Private analysis reported three redirected attempts reaching the 600-second limit without committing a staged handoff. | R1: reserve finish time and expose uncommitted recovery evidence; never blindly replay uncertain effects. |
| Steering records reported useful progress, repeated no-change replies and workload failures. | Preserve coordination; isolate generic harness defects from workload/protocol issues. Retained services after cancellation alone do not prove an ownership bug. |
| Current source: `history::activity` defaults failed reads to empty; HTTP accept spawns a handler thread per request. | R3/R4 remain concrete reliability work. Model slot limits do not bound HTTP threads. |
| Current source: Lens selection uses `NoSummaries`; the server still owns broad mutable `State`. | C1 and A1–A4 remain real gaps despite journal/core-crate progress. |

Preserve shipped journals/outbox, ancestor request visibility and #134 continuity
composition. Historical planning labels are not current status; callbacks already
exist without a full off-lock writer. New work follows current crate contracts.

## P0: continuity and observable failures

| ID | Small implementation slice | Acceptance |
| --- | --- | --- |
| R1a | Derived active/finishing/ended phase, with an initial measured finish reserve of 60 seconds within the existing 600-second limit. Advertise it on ordinary responses without needing a callback. Fence new host workload mutations while allowing bounded status, cancellation and handoff. | Fake-clock boundaries, no new mutating host work in finishing, successful publication for both CLI providers; ignored wind-down still times out normally. Native CLI private tools and independently running services are outside this fence. |
| R1b | Retain failed generation, last committed checkpoint, termination cause and draft/log references as explicitly uncommitted recovery evidence. Present it at the next authorized turn. | Failed publication leaves committed notes/cursors intact; missing drafts visible; reconciliation markers commit with the next matching checkpoint. Cancel/pause does not auto-retry; ambiguous effects are inspected, never blindly replayed. |
| R2 | Select relevant events and reconcile known intent/observations before admitting a turn. Retain full watch output; exit-only or bounded literal-prefix-plus-exit policies provide an incremental filter for untyped streams. Preserve provenance, source sequences and eligible references. | Unchanged status-query/own-output telemetry causes zero admissions. Human steering, relevant child/tool completion, failure and source loss remain visible. Restart, stale observations, coalescing and missing spool tested. Reading is not acknowledgement. |
| R3 | Distinguish missing/corrupt history from an empty activity record in UI and model context. | Missing/decode-failed records preserve cause and references; ordinary empty records remain empty. |
| R4 | Bound HTTP handler admission before spawning, with a bounded overload path and existing deadlines. | The implementation PR must specify numeric handler cap H and request/overload deadlines from measured memory and slow-client fixtures. Test H+1 admission, recovery, shutdown and long-lived routes; no unbounded rejection threads. |

R1a/R1b form the recovery increment; R2 is independently testable. R3 and R4 can
ship as isolated fixes. Keep typed delivery-retirement bookkeeping out of task
context while retaining lifecycle records. Continue the existing shared agent
contract: a bot adds approved scheduling, not a different memory/runtime system.

R2 targets `src/bots.rs::watch_events` and its `Event::Info { from: None }`
delivery. `Watch` already retains chat/service/generation; line aggregation loses
causal detail. Carry source/resource generation and, where known, producing
attempt/call identity through admission. Unknown origin stays unknown; owning a
stream does not prove who caused each line. Keep event kind and provenance
separate from room-post echo suppression. Simply setting `from` to the owning
chat would drop its exits too: the supervisor checks `own(from)` before urgency.

Classify typed events in the tool/application adapter; retain configurable
exit/prefix selection for raw streams whose semantics the host cannot know.
Test an unchanged status echo alongside the same owner's completion/failure,
unknown-source output, restart and missing spool. Preserve full evidence and
processing cursors. Deterministic admission needs no goal-interpreting model;
unknown/stale observations are not success. This fix does not depend on G1.

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

## Supporting contracts, delivered with their consumers

**G1: one lifecycle authority, narrow projections.** Reuse `Definition`, `Restart`,
bot generation and `Supervisor::status()` in `crates/bot/src/supervisor.rs`.
`Status { Ready, WaitingTurn, Paused, Stopping }` is derived lifecycle state;
do not persist a second status or force product conditions into that enum.
Extend these primitives only for a required generic transition. E1/U1 first need
application projections of NeedsDecision from durable requests and
WaitingDependency from existing wait/continuation records, with generation,
last transition and source references. Goal/persona prose stays outside the core.

Use separate application types for host observations and agent progress claims.
Only host-owned writers construct observations from authoritative records;
model input constructs an attributed claim with checkpoint/evidence references.
An active process is observable; useful goal progress is not established by that
fact. UI shows the distinction and missing/stale data. Tests prove claims alone
cannot resolve requests, advance lifecycle or overwrite observations; duplicate/stale
deliveries cannot produce a second turn. Add further conditions only for a consumer.

**O1: audit existing ownership at the resource boundary.** Reuse the persistent
resource transitions in `control-core` and their application/engine adapters.
Chat parentage is not resource ownership. First reproduce a lifetime mismatch or
identify a new feature changing cancellation/cleanup semantics; then add the
smallest needed ownership or retain/cascade operation. A persistent service left
after `/cancel all` may be correct. Test owned versus borrowed/shared services,
cleanup failure and restart. No general garbage collector blocks E1 or U1.

**Q1: start with shared counters.** Aggregate existing `Budget` window/turn limits
and runtime slot admission by team in the application admission operation, with
durable charges keyed by attempt. Capture team identity through child/retry work;
spawning or redefining a bot must not reset team charges. Use pure counter helpers
only where needed; currency pricing and team configuration stay out of the supervisor.
For CLI adapters, this slice enforces turn/concurrency limits only: there is no
in-flight token or cost ceiling. Expose unknown usage separately from live capacity.

Release a slot on confirmed termination, regardless of whether usage arrived.
On recovery, inspect/fence the old attempt before reusing its capacity. A stale
reservation has a reconciliation deadline and visible operator action; expiry
triggers reconciliation, not an assumption that the process stopped. Later
adapter-specific usage reservations require a bounded settlement policy: close
unknown usage as an explicit conservative charge or audited operator adjustment,
retain a tombstone for late usage, and never silently refund spend on timeout.
Tests cover absent/late usage, crashes, concurrent admission and idempotent release.

Add non-preempting queue priorities when contention demonstrates the need:
interactive/decision work before workers and summaries, with fairness/aging.
Priority cannot create capacity or bypass grants; test one-slot parent/child
resolution. Increased unattended team fan-out needs Q1; a bounded W1 prototype
can use existing budgets and slots without completing this later scheduling work.

| Placement | Responsibility |
| --- | --- |
| Pure `control-bot` / domain core | Existing generic lifecycle, waits, counters, generation fences, resource transitions and typed effects; no goal, recipe, persona or model-specific policy |
| Narrow application operations / projections | Authorization, atomic acceptance, team accounting, host observations versus claims, request resolution and UI views; no second lifecycle store |
| Standard-library tools, strategies and configuration | W1 cron interpretation, W2 recipes, E1 decision tools, L1 persona/SOUL bundles, learning procedures and C1/C2 compaction strategies |
| Effect / infrastructure adapters | Persistence, clocks, model calls, process/container work and resource inspection; interpret typed effects and revalidate authority |

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

Project a pending request into G1's NeedsDecision application view with its request
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

Read authoritative lifecycle and existing ownership directly; use G1 projections
for requests/waits as they ship. Show observation age and unknown progress.
O1 is needed only if a concrete ownership gap prevents that view or operation.

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
policy applies per schedule; existing admission limits remain effective. Allow
creates independently owned instances and never concurrent writers in one chat. Replace requires
confirmed cancellation/quiescence of the prior writer and reconciliation of its
effects; defer enabling it until that contract is tested. Show skipped occurrences
and reasons. Initially choose at most the latest eligible missed occurrence within
the deadline; document this Control catch-up policy rather than claiming complete
Kubernetes equivalence. Specify daylight-saving gaps/repeats and test previews,
restart, clock changes, edits and pause racing a due trigger. A deduplicated trigger
does not make uncertain external effects safe to replay. Schedule ticks never
restart a progressing provider attempt; a later admitted attempt starts fresh.

First W1 keeps schedule creation/edit/enable operations operator-only. All writes
call one typed application operation that checks authenticated operator authority
and rejects agent-attempt credentials, regardless of transport or caller-provided
origin fields. Agents can propose definitions for operator activation. This narrow
A1 operation ships with W1; a complete transport refactor is not required. Tests
cover direct agent MCP calls, child/continuation attempts and all exposed write
routes. Messages cannot confer schedule-write authority. An operator acting on a
proposal is a new authorized action. Agent-managed schedule writes are deferred
until durable capability restrictions survive delegation and continuation; do not
claim a universal message-origin guarantee. Hermes' configurable
[cron guard](https://hermes-agent.nousresearch.com/docs/user-guide/features/cron)
is a reference, not the enforcement mechanism.

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
Implement interpretation as a workflow tool over spawn/message/wait operations;
persist its recipe-specific state outside the supervisor. Add G1/O1/Q1 pieces
only for the decisions, resource lifetimes and fan-out the recipe actually needs.

First acceptance: a scheduled inspect/build/review recipe survives a restart and
a human decision wait, posts one outcome and leaves unrelated work running.
Ad hoc teams continue to work without recipes. Keep generalized workflow languages,
distributed scheduling and arbitrary executable hooks outside this first layer.

## Supporting work: decouple state and execution incrementally

Use A1–A4 to remove coupling encountered while delivering the product work above.
Each feature must establish its narrow boundary before adding behavior. The full
extraction is not a prerequisite, and broad `State` access is not an acceptable
shortcut for a new tool simply because that coupling already exists elsewhere.

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
  versioned node defaults/advanced configuration with permitted per-chat overrides
  and existing admission limits (Q1 for team aggregation). Use existing context seams, with
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

Preserve child-result lineage, typed activity, room discovery, approval and
continuity regressions. Reproduce historical failures against main before
reopening them. Follow measured knowledge/transcript/polling hot paths; keep disk
quotas, launch-recording crash gaps and relay memory limits as separate reliability
work. The #138 FIN-drain fix is merged. Preserve plain-file storage and explicit
migrations; federation and inference bundles remain later work.

Each implementation PR states its ID, observed problem, contract changed,
dependencies, checks and remaining limits. Run the relevant
[contribution checks](../CONTRIBUTING.md); engine changes additionally need live
resource tests. Fault/cancellation tests precede claims of recovery correctness.
Planning-only changes require document/link/diff checks, not model calls.

For R1/R2, first publish credential-free synthetic deadline/watch regressions.
Then compare fresh isolated workloads with pinned revisions/models/resources
and identical objectives. A Minecraft replication can use seed
`-6834438708894114399`; preserve the existing playtest. Compare deadline and wake
changes separately before combining; label a combined-only run exploratory.
Observe 30 minutes of maintenance after completion, then the same redirected work.
Record milestones, useful actions, admission causes, deadline/publication failures,
committed-state age, context bytes, reported usage and missing usage. Report sample
size and response to real failures/new steering, not just reduced wake count.

Use the delivery table above for sequencing. C1 pure cache/cut fixtures and the
first reviewed learning demonstration can proceed without the full model or
architecture extraction. Measure delivery quality, coordination, intervention,
noise and reuse. This roadmap does not implement or deploy runtime changes.
