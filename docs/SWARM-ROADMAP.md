# Persistent swarm and self-improvement roadmap

Triaged 2026-11-05 against main `2d3492e` (#138). This is a plan, not a runtime
change. [The main roadmap](../roadmap.md) tracks shipped capabilities and release
gates. Stable IDs below identify independently reviewable increments.
Status audited 2026-11-10 against locally deployed main `52bff4c` (through #148).
The checklist distinguishes shipped slices from remaining acceptance work;
unmarked product proposals remain planned. The native Nix package and pinned
WASM tooling fix shipped in #143; both platform checks passed before merging.

## Shipped status and next increment

- [x] **R1a lifecycle slice:** approved persistent bots have no implicit 600-second
  wall-clock cap; explicit operator limits remain enforced. Accepted handoffs
  finish through host-owned stop/publication paths, and event-only waits release
  model capacity. Both CLI loops use the shared attempt policy; attempt-owned
  commands cap their deadlines to the owner and expose requested/effective limits.
- [x] **R1b implementation:** immutable unpublished recovery candidates retain
  workspace bytes, base/generation, termination cause and accepted drafts after
  verified writer shutdown. Scoped inspection/recovery tools do not advance the
  published head or processed cursors. Capture/rejection fixtures exist; this
  successful unattended replay does not exercise an injected recovery fault.
- [x] **R2 admission slice:** configurable watch exit/prefix selection, retained
  spool evidence, source-gap reporting and generation/creator references; scoped
  task rooms, record-versus-notify delivery and targeted peer communication.
  Idle peer wakes use existing bot approval, budgets, pause and stop intent.
- [x] **Steering-chat relay removal:** workers can coordinate directly with
  siblings; the root conversation need not be a bot or relay their routine work.
- [x] **R2 explicit callback channel (#146):** opt-in watches deliver application
  notifications separately from diagnostic stdout/stderr; process exits remain
  host-observed facts. Default callback reads omit logs; explicit diagnostic
  reads retain them. No workload-specific parser or timeout was added to core.
- [x] **Opt-in autonomy/investigation lenses (#144):** declarative guidance is
  captured with runs; adding a lens does not grant resource or tool authority.
- [x] **First typed message operation (#148):** borrowed application views and
  ordered archive/checkpoint/wake effects, with blocking/Tokio conformance tests.
  Shared conversation/run records no longer belong to rendering modules. The
  production driver retains its cross-domain lock through persistence.
- [x] **Efficiency checks for #142:** routing benchmarks, flamegraphs and bounded
  resource probes are recorded in [PERFORMANCE.md](PERFORMANCE.md). The tiny-device
  goal concerns efficient core/tools/TUI and replaceable backends, not a mandatory
  Raspberry Pi certification or a claim that every HTTP path became faster.
- [ ] **R1/R2 remaining evaluation and classification:** paired interruption/recovery
  comparisons and finer causal classification of raw watch output. Creator identity alone does
  not establish which attempt caused each line. Deliver these only as narrow
  tool/adapter fixes when needed; do not add workload semantics to the supervisor.
- [ ] **R3/R4:** visible history-read failures and bounded HTTP admission remain
  open. They can ship independently alongside the architecture slice.
- [x] **Minimum architecture checkpoint: first A1/A2/A4 boundary**, then
  consumer-led A3. The directed-message increment in #148 establishes the first typed
  transition/effect boundary, with a blocking production interpreter and Tokio
  conformance fixture. Out-of-lock persistence, broader callback admission and
  complete web/TUI separation remain open; see [STATE-BOUNDARIES.md](STATE-BOUNDARIES.md).
  This follow-up separates the shared MCP client from terminal presentation,
  compiles terminal code once and provides an optional server-only build. The
  independent crate split and web rendering boundary remain subsequent work.
  The storage adapter now exposes phase-aware checkpoint errors to distinguish
  pre-replacement failure from unconfirmed rename/directory sync.
  The adapter reconciles exact published bytes and durability on uncertainty;
  an unresolved legacy save uses existing shutdown/state-unavailable fencing.
  Startup confirms disk state before journal recovery. This does not yet make
  out-of-lock or cancellable persistence safe.
  Typed client operations now own port/project approval transport and confirmation;
  terminal presentation cannot access production HTTP-client fields. This closes
  the immediate separation increment, not every A1–A4 migration. Complete web
  extraction, independent client/TUI crates and out-of-lock persistence remain
  follow-ups. Extend boundaries with their consumers rather than adding more
  general architecture ahead of features.
  Continuation construction also uses the shared application `Submission`
  constructor with captured settings and host-supplied grants/IDs. Approval
  eligibility is a borrowed application decision; authorization and the complete
  request acknowledgement/publication transaction remain in the host.

The fresh deployed Codex trial ran over two hours after approvals without further
operator steering: house/perimeter verification, repeated crop harvest/replant,
bread production and recovery from game hazards. At the audited checkpoint it had
52 completed attempts, zero failed attempts, 29 record-only room posts and no root
turn after setup approvals. Individual useful attempts lasted about 26 and 32
minutes. This supports continuity and quieter coordination; it is not a paired
speed, token-cost or injected-fault benchmark. House size, starting location,
client versions and inherited knowledge differed from the original run.

**Deployment usability:** #143 shipped the native Nix package and NixOS
instructions. The normal setup path should build/install the package, load images
and save immutable pins in one command; manual build/pin steps are an advanced
reference. Provider login and optional service activation remain explicit.

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
The sequence is **small diamond-run reliability fixes → state/effect abstraction
→ broader improvements**. The shipped R1/R2 slices above are the baseline; preserve
their regressions and address remaining reproduced failures in small changes.
Do not make further gameplay tuning or a perfect run a prerequisite for the state
boundary. Use the paired-trial gates below for comparative performance claims.
Before expanding product behavior, prove the first A1/A2/A4 operation boundary
with interchangeable drivers; complete the remaining extractions incrementally.

| Priority | Work | Minimum dependency / first exit |
| --- | --- | --- |
| Shipped baseline / remaining P0 | R1a/R1b and R2 slices above; remaining R1/R2 edges, R3 history errors and R4 HTTP limits | Preserve shipped behavior; finish independent fixes with fault tests and real alerts. |
| Established baseline; extend with consumers | A1/A2/A4 first state/effect slice, then incremental A3 and remaining boundaries | Typed message/effect conformance, captured continuations, checkpoint reconciliation and the client/server build boundary are established. Broader web/client crate extraction and out-of-lock persistence remain open, without blocking the next feature. |
| Next feature | M1 summary-call adapter, then C1/C2 compaction | One bounded text request/result/usage contract; configured hosted/local adapters, bounded history reads for summary jobs, independent compactor selection and recall/failure/performance fixtures. Full local tool-using agents follow later. |
| P1 | E1 escalation with G1 request/dependency projections | Existing lifecycle and durable requests; one-slot resolution works. No broad spec/status subsystem. |
| P1 | U1 coordination UI and A2A visibility | Read existing lifecycle, requests and ownership; add projections as needed. No dependency on all G1/O1/Q1 work. |
| P1 | T1 terminal development workflow | Complete existing typed tool/query/approval surfaces for terminal review, PR/check follow-up and browser/process interaction. Keep policy in tools/adapters; see [terminal gaps and acceptance](TERMINAL.md). |
| P1 | W1 bounded schedules | Existing timers/admission, durable occurrence IDs and shared schedule-write authorization. Q1 before expanding team-wide fan-out; Replace deferred. |
| Supporting | Q1 team admission; O1 ownership audit/fixes | Aggregate existing counters; extend ownership only for a demonstrated gap or a consumer changing resource lifetimes. |
| P1, before more fan-out | S1 service resource requests and admission | Extend existing service operations with requested/effective capacity and operator-defined shapes; reserve aggregate node/team capacity before provisioning. Local backend first, cloud adapters later. |
| P1 | V1 Manage Vault and scoped secret injection | Typed tool/resource operations and host authorization; agents use opaque references, with values resolved only inside trusted effect adapters. No supervisor credential subsystem. |
| P2 | U2 CUA; W2 workflow tools | Scoped browser/resource operations; validated recipes using existing spawn/wait/message primitives. |
| P2 | M1 model adapters; C1–C3 summary strategies | Bounded reads and summary-call adapter enable C1 before general local tool execution; on/off plus model selector. |
| P2 | L1 lenses/persona/SOUL configuration; L2/L3 reviewed improvement | Captured revisions, scoped tools, evaluation and admission; no new supervisor personality or learning lifecycle. |
| Later | Federation, inference bundles, general workflow languages, weight training | Demonstrated local need. |

## V1: Manage Vault and scoped secret injection

Provide a Vault in **Manage** for operator-owned API keys, tokens and other
credentials, with the requested GrokBot-inspired workflow. Operators add, rotate,
revoke and grant use without pasting secrets into chat. Agents can discover only
permitted metadata and request an opaque secret reference; values never enter
model context, steering/room chat, tool-call arguments visible to the model,
transcripts, summaries, knowledge, recovery candidates or ordinary logs. The
private operator input flow must also stay outside agent-visible browser/CUA
surfaces and captures. Do not echo a newly stored value back into chat.

Resolve references at the trusted tool/API/service effect boundary, after checking
the current caller, owning team/chat, attempt generation, permitted destination,
operation and expiry. Support broker-attached API authentication (for example an
authorization header) and explicitly approved service bindings. Tool schemas
carry references, not literal keys. Do not expose a secret-reading tool, put keys
in URLs, or inject raw values into an agent's shell/environment/workspace: the
agent could read and print those values. If a tool runs arbitrary agent-authored
code, use a credential-bearing broker outside that process. Plain redaction is
defence in depth, not the isolation boundary; constrain destinations/redirects
and filter secret-bearing or reflected responses before returning tool output.
Return bounded typed response projections; raw request/header dumps must not
become model-visible tool results. A secret-bound service must not also grant
agents arbitrary exec/read access to its credential-bearing process or files.

Keep values outside the checkout and ordinary application state/journals, using a
platform secret store or encrypted private files with separately protected key
material. Specify backup/recovery, rotation, revocation and per-node ownership.
Audit who used which reference, for what operation/destination and with what
result, without logging the value or authentication payload. Grants are checked
at execution, not only registration; queued calls and persistent services must
respect revocation. An API credential or payment credential grants no implicit
permission to spend or perform another consequential action: use existing durable
operator approvals and action-specific limits where required.

Implement the Manage UI, secret storage and injection as application operations
and trusted adapters over existing capability/resource semantics. Reuse generic
typed authorization/effect interfaces; keep provider formats, payment workflows
and credential resolution out of the supervisor. Declarative tools/configuration
select bindings, including dynamically admitted tools, without receiving values.

**First exit:** create a synthetic secret through Manage, let an authorized agent
call a local test API by reference, and prove the API received authentication
while model input/output, chats, tool traces, logs and persisted candidates did
not contain it. Exercise cross-agent misuse, wrong destination/redirects,
reflected response bodies, expired/revoked grants, queued-call revocation,
rotation and denied CUA access. Measure dispatch/memory overhead with synthetic
values; never use real credentials in fixtures or CI. This is planned work.

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
| Private analysis reported three redirected attempts reaching the 600-second limit without committing a staged handoff. | R1: trace checkpoint/yield boundaries and retain recovery evidence. This does not establish that an earlier forced wind-down improves progress. |
| Steering records reported useful progress, repeated no-change replies and workload failures. | Preserve coordination; isolate generic harness defects from workload/protocol issues. Retained services after cancellation alone do not prove an ownership bug. |
| Current source: `history::activity` defaults failed reads to empty; HTTP accept spawns a handler thread per request. | R3/R4 remain concrete reliability work. Model slot limits do not bound HTTP threads. |
| Current source: Lens selection uses `NoSummaries`; the server still owns broad mutable `State`. | C1 and A1–A4 remain real gaps despite journal/core-crate progress. |

Preserve shipped journals/outbox, ancestor request visibility and #134 continuity
composition. Historical planning labels are not current status; callbacks already
exist without a full off-lock writer. New work follows current crate contracts.

## P0: continuity and observable failures

| ID | Small implementation slice | Acceptance |
| --- | --- | --- |
| R1a | Trace and harden existing callback, explicit yield/pause and checkpoint-publication hooks. Deliver changed input at tool boundaries; preserve productive attempts. Request/stage handoffs at genuine dependency, decision or completion boundaries, then commit through the existing safe publication transaction. | Both CLI providers process useful events without forced restarts; duplicate callbacks create no extra turns; checkpoints commit on normal yield/pause; abrupt termination retains honest recovery evidence. No fixed 60-second finishing window or time-triggered blanket mutation fence. |
| R1b | Host-owned recovery: after confirming attempt writers stopped, retain workspace bytes as an immutable unpublished candidate with generation/base references, termination cause and any accepted draft/logs. Surface it on the next authorized turn without relying on model warnings. | A provider making no Control calls still leaves inspectable recovery work after termination. Copy/save/stop failures stay visible; committed workspace, notes and cursors remain unchanged. Cancel/pause does not auto-retry; adoption revalidates authority and current baseline. |
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

R1 starts with existing `turn_context::callback_notice`, handoff tools and worker
finish/design/delegation publication paths. Reproduce the lost-handoff case and
fix its boundary; do not add a generic hook framework. Hooks here are typed Rust
integration points over existing operations/effects. Tool results, dependency
completion/failure and steering supply events; raw telemetry need not call a model.
An active attempt consumes relevant deltas at tool boundaries. When it genuinely
yields, existing admission can schedule an authorized successor from committed
state. An event alone does not restart the current writer.

A handoff draft or callback is not a committed workspace. Preserve the fence
after an explicitly accepted yield and quiesce attempt-owned writers before
atomic publication. Do not snapshot a changing workspace on every tool response.
R1b covers crashes before that transaction; callbacks cannot guarantee a final
checkpoint after abrupt process loss. Independent persistent jobs keep running:
record their handles and completion subscriptions, then resume by reference.
Large downloads, for example, belong to a resumable tool over persistent resource
operations; download protocols and model-weight semantics stay outside the core.

Current timeout paths already retain the run directory; R1b makes those bytes
durably discoverable and inspectable as recovery work. Capture after verified
writer shutdown, not at a timed reserve while native tools can still mutate files.
Use existing artifact/project storage boundaries with a separate candidate
reference; preserve exclusions and expose retention/capacity failures. Do not
change the published head or promote a handoff's processed cursors. Record the
latest accepted draft separately: it may describe an earlier state than the final
files. A stable copy may still contain a partially written file or unfinished
multi-file change; it is evidence to inspect, not proof of successful execution.
If quiescence cannot be confirmed, retain the source as unstable and report why
no stable candidate exists. Restart recovery can retry capture after inspection.
Candidate adoption into fresh work rechecks the current baseline and authority;
external service/world state must be inspected separately. A Minecraft world
living in a service is not restored by copying an agent workspace.

Record a structured timeout cause and the captured effective deadline on the
attempt, independently of the broad failed/interrupted lifecycle label. Preserve
that primary cause if cleanup also fails. No new supervisor state is needed just
to let U1 or accounting distinguish timeout from provider error or cancellation.

`control_workspace_start` jobs are attempt-owned, unlike persistent service jobs.
Their timeout is an upper bound, not a predicted completion time. Use the same
captured attempt deadline as the provider loop: cap the effective job deadline at
the owner's remaining lifetime, disclose requested/effective limits and owner
termination in start/status responses, and reject launch once no time remains.
Revalidate at launch after queue/setup delay. Do not reject an otherwise useful
short command merely because its requested maximum exceeds the remaining time.
Long independent work uses persistent resources with separate limits. Test the
minimum 60-second configuration, late launch, cancellation and independent jobs;
none requires reinstating an early finishing reserve.

Keep bounded backstops for cancellation, hung providers and transport operations,
with distinct scopes and visible configurable defaults; they need not require
an operator to opt in on every call. Capture the effective limit and termination
reason. Distinguish request deadlines, cancellation grace, inactivity detection,
attempt budgets and persistent-job lifetime. Silence alone does not establish a
stall; a heartbeat or log line does not establish useful progress. Tool adapters
choose operation-appropriate bounds and report them through generic outcomes.
Ordinary attempts retain a default 600-second cap; approved persistent bots now
have no implicit wall-clock cap, while explicit configured limits remain binding.
This is an attempt policy, not a task/service lifetime or a universal target.
Remaining allowance may be reported; no periodic handoff timer or additional short
deadline controls ordinary progress. Any cap change needs measured recovery and
capacity results and cannot silently extend an authorized budget. Timeouts remain
a failure/resource backstop rather than the workflow.

Long-job tools may expose a bounded health/progress inspection hook: resource
identity/generation, liveness, last observed progress and evidence. Keep workload
semantics in the tool; an alive process does not prove progress, and missing metrics
mean unknown. Healthy, progressing work continues within its authorized limits.
Stale/unknown progress triggers a bounded recheck or decision request, not an
automatic claim of failure. Inspection failure is distinct from job failure.

Expose effective runtime/inactivity limits, review interval and expiry action at
job creation and inspection; allow authorized changes per tool/job within node
policy. Before a configurable limit is reached, request more time through E1's
existing design-question flow when needed: lead within delegated scope, otherwise
operator. Apply an extension through an authorized host operation; answer prose
alone cannot change a budget. While awaiting a decision, the captured policy
governs continuation and expiry; silence does not grant more time. A quiet long
calculation may succeed, so test it alongside a hung process, unavailable health
hook and unanswered extension request. These hooks extend the relevant tools,
not core job-specific states or a prerequisite health framework for R1.

Change workload guidance separately: act while useful work remains, check deltas
at meaningful boundaries, publish on explicit yield/pause, send changed facts/milestones/
blockers, and wait after completion. A lead intervenes for changed goals,
dependencies, evidence or stalled progress. Routine audits should not repeatedly
nudge a productive builder. Preserve the successful long active slices.

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

**S1: agents request service resources through tools.** Let an agent request,
for example, a 24 GiB Minecraft container or an inference service with GPU access.
Extend existing service operations with typed CPU, memory and accelerator requests;
operator-defined shapes are optional presets. Separate compute size from image
capabilities (`Lite`/`Full`), which currently fix memory at 512 MiB/2 GiB and cap
every service at one CPU. Keep existing defaults compatible.

Admit requests within configured capacity and grants; otherwise return a clear
capacity result or use the existing durable approval path. Approval cannot create
missing RAM or a GPU. Report requested/effective limits, accelerator availability
and bounded utilization/pressure through inspection tools. Account for concurrent
workers and services on the same backend. GPU access is an explicit device grant,
not privileged mode or arbitrary device passthrough.

Agents can also start a model server as an ordinary persistent service and call
it through a scoped tool/MCP endpoint. Model selection, downloads, inference
configuration and MCP wrapping belong to tools, with network/device grants as
needed. This can ship before M1: calling a model as a tool does not require using
it as Control's agent backend. No model-serving lifecycle belongs in the supervisor.

Keep admission and ownership generic, with provisioning interpreted by replaceable
backends. Agents choose approved tools; provider-specific cloud APIs, instance
types and credentials stay in those tools/configuration. Borrow Kubernetes'
intent/status/reconciliation boundary, without building a cloud scheduler. Adding
a provider must not change pure core or the bot supervisor. Remote capacity and
spend authorization are scoped to that backend.

First deliver local resource requests, persisted effective limits and lifecycle
accounting. Test concurrent/duplicate admission, unsupported CPU/GPU requests,
restart reconciliation and cleanup after an agent stops. Release reservations on
confirmed teardown; reconcile unknown provisioning outcomes visibly. Resizing an
existing service must explicitly report live-update versus replacement support
and preserve data and port/grant generation fences. Measure matched-load latency,
CPU throttling and memory pressure before claiming an improvement from more RAM.

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

**E1 extends `control_design_ask`; it does not replace it or add a second request
system.** Reuse [design::Question and Continuation](../src/design.rs), request
identity/generation checks, durable pause, worker-slot release and checkpoint
resumption. Existing `pending`/`answer_target`/`answer` operations already support
ancestor-visible questions and answers to descendants. The missing increment is
an agent-callable, scoped lead-resolution tool over that same application path.

Add only the resolver reference/scope and answer attribution needed for that
extension to the existing question record. Old records and ordinary calls retain
operator resolution by default. A worker may route a delegated task decision to
its assigned lead; host checks recorded lineage and delegated decision authority
at resolution time. Naming a resolver does not grant authority. The answer tool
must use the existing validated answer and durable-save/rollback boundary, retaining
checkpoint, workspace-baseline, grant, budget and continuation rules. Do not expose
the internal answer function without those checks or introduce supervisor variants.

Unresolved human preferences and decisions outside the lead's mandate go to the
operator through the existing UI/Discord path, preserving the question identity.
Access requests continue through the permission broker; procedure/knowledge
proposals continue through knowledge review. Those are separate existing flows,
not additional E1 resolver types, and a design answer never grants access.

G1's NeedsDecision view references the same `Question`; changed questions reach
the lead through existing delivery. Keep the existing request deduplication and
single-pending-question semantics. Ordinary collaboration still uses steering,
room messages and follow-ups; a decision question pauses only the affected work.

A parent waiting on its child must be able to receive and resolve that child's
request. Test legacy records/default operator routing, the one-model-slot case,
a full queue, restart, duplicate/stale
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

### Opt-in investigation when a plan is blocked

Start with a versioned procedure/lens enabled per task or team, using existing
inspect, execute, message, wait and checkpoint tools. Routine troubleshooting is
already encouraged; this adds a concrete method, not a mandatory experiment on
every wait. Implement after the first state/effect slice. Add a thin tool only if
the procedure cannot retain or inspect trial records through existing operations.
No new supervisor mode, ambient state handle or generic experimentation framework.

The procedure records the blocker and source evidence, a hypothesis, the smallest
authorized probe, expected/disconfirming observations, and the actual result.
Use a bounded number of probes and existing task/resource/model budgets. Compare
outcomes before retrying; an unchanged guard rejection is not a new experiment.
Choose an alternative, request targeted peer help, or use E1's existing decision
path when the next action exceeds the mandate. Valid dependency waits remain
valid; elapsed silence alone neither diagnoses a stall nor authorizes intervention.

Keep trial notes with the task's checkpoint/evidence and resume from them, rather
than rediscovering failed approaches. Probe state and workload-specific health or
risk judgments belong to the procedure/tool. Existing host operations enforce
grants, cancellation and publication; a hypothesis, lens or peer suggestion adds
no authority. Avoid broad broadcasts or model wakes for every sample.

Acceptance: an unchanged safety/dependency guard leads to a distinct evidence-led
probe or explicit blocked outcome instead of endless identical retries; a healthy
long-running dependency continues undisturbed; failed/interrupted probes preserve
notes and uncertain outcomes; exhausted budgets and out-of-scope actions use the
existing wait/request paths. Evaluate whether the procedure reduces repeated
actions without increasing intervention or noise. Verified general lessons may
be proposed through L2 review; one successful probe is not automatically knowledge.
Minecraft combat/despawn rules remain workload knowledge, outside Control's core.

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

## State/effect boundaries: established baseline and incremental follow-up

The first A1/A2/A4 operation boundary follows the small diamond-run fixes and is
now established by the slices above. A1–A4 remain directions for consumer-led
work: typed input, domain state transition, persisted intent, interpreted effect
and typed completion. Reuse the existing bot machine and application adapter;
do not invent a second supervisor. Keep the conformance gates below for each
migrated operation. Finishing every extraction or web/TUI crate split is not a
prerequisite for M1 and compaction. New features use the proven boundary and
extend it where needed; broad `State` access is not an acceptable shortcut.

The useful state-monad-like property is explicit composition: a transition maps
domain state plus typed input to updated state and effect descriptions. Lift a
domain transition into its owning application operation through narrow borrowed
views and typed effect mapping; do not give tools an ambient global-state handle.
The existing `Machine::step` mutation interface can express this without cloning
all state, a generic monad framework or adding feature-specific core concepts.
Effects carry their resource/generation/revision context. Interpreters own I/O;
results return as typed events and stale completions cannot mutate newer state.

Keep clocks, filesystem/process handles, channels and executor types in drivers.
The first slice must run through the existing driver and one experimental async
driver against the same fixtures, with no runtime dependency in its domain types.
Check ordering, duplicate delivery, cancellation, restart, save failure and
durable-before-effect behavior, including concurrent completion. This proves a
runtime can change without changing domain semantics; it does not establish that
an async runtime is faster. Benchmark before choosing a production replacement.
Record which full-state accesses the slice removes and retain the diamond-run
fixtures as behavior regressions.

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

## M1: shared model calls, roles and local inference

M1 defines separate captured selections for worker, lead, reviewer and compactor.
S1 can independently expose an agent-started model service through tools/MCP;
M1 concerns using a model to execute Control's own agent or summary attempts.
Start with a bounded model request/result/usage interface usable by the summary
worker, then add native API and configured local inference through Rust adapters.
CLI providers continue to work. A provider choice does not change tools,
permissions, workspace publication or the supervisor.

This is the next feature increment after the client approval boundary. Keep one
model-call contract and a shared registry, with CLI, hosted API and local endpoint
adapters; do not add a crate per model or require a runtime rewrite. List the
configured models in the UI, including local models and available low-cost
Codex/Claude selections. Capture provider/model and capability metadata rather
than treating every listed model as interchangeable or assuming account access.

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

The selector must also include configured Codex and Claude CLI models that can
run a bounded text-only summary request without tool execution rights. Reuse
CLI execution/normalization adapters and record availability and usage limits;
do not create a separate summarizer implementation for each model. Low-cost
choices should be selectable for compaction as well as ordinary model selection.
Validate current availability, total cost and summary quality rather than
hard-coding a permanent "cheapest" model. Never silently substitute a cloud model
for a local-only selection.

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

**Paired evidence gates comparative R1/R2 performance claims.** The deployed
slices and the unattended observations above do not establish those claims.
First publish credential-free regressions for the specific defect: unchanged
watch echoes with genuine exits, or timeout with useful files and no Control tool
calls. Then run baseline and changed revisions in separate fresh worlds from seed
`-5872320857145672064`, with the same steering script, agents, model configuration,
tools, budgets and workload version. Preserve the successful original world.
Verify generated-world identity, spawn/start conditions and milestone size as well
as the configured seed; this replay visibly differed from the original despite
the requested seed. Verify milestones from world/tool evidence. Each experiment
changes one factor: watch admission, recovery candidates, or attempt-limit policy.
Repeated paired trials are needed to assess model variability; a single successful
run is a smoke test. Record run count and pinning/availability limitations.

Predeclare the targeted gain and acceptable variation before comparing runs.
R2 must reduce redundant maintenance turns while preserving milestone completion
and response to real failures/steering. R1b must preserve recoverable bytes after
the same injected interruption and reduce repeated work or time to resume useful
progress; separately measure candidate capture/inspection overhead. Test natural
uninterrupted runs too, so an injected fault alone cannot hide a regression.
Track time to diamonds, milestones, useful actions, repeated actions, human
interventions, checkpoint loss, context bytes and reported/missing usage. Observe
30 minutes of maintenance after completion, then apply identical redirection and
a real fault. Include a persistent job spanning model attempts. Fewer wakes or
more saved files alone do not prove better diamond-run performance. Mixed results
remain experimental; do not present planned mechanisms as proven improvements.

Use the delivery table above for sequencing. C1 pure cache/cut fixtures and the
first reviewed learning demonstration can proceed without the full model or
architecture extraction. Measure delivery quality, coordination, intervention,
noise and reuse. This roadmap does not implement or deploy runtime changes.
