# Roadmap

This file lists work Control does not do yet. Shipped behaviour is described in
the reference docs ([README](../README.md), [architecture](ARCHITECTURE.md) and
the topic pages it links), not here. Stable IDs (R, A, E, G, …) name
independently reviewable increments.

## Direction

Control is a **swarm management platform**: a person directs a team of agents
that plan, build, review, test and operate systems. Agents specialise through
lenses, personas, tools and model choices; chats retain their work; rooms
coordinate; services keep running between turns; admitted knowledge carries
lessons between tasks. Deterministic Rust supervision handles scheduling,
ownership and budgets. Replacing a whole engineering team is an ambition to
validate on real delivery and operations tasks, not a demonstrated capability.

Keep the pattern that works: a steering chat sets direction, a lead coordinates,
workers talk to each other through rooms and directed messages, and persistent
services hold the workload. Improve it with measured reliability and noise fixes.

**Layering is the architectural constraint (Pi/tinygrad style).** Keep a small
pure vocabulary for lifecycle, admission, waits and typed effects. Build product
behaviour as tools, strategies and configuration over narrow application
operations; adapters own I/O. A new feature does not justify a new core concept:
each PR that changes a core API must name the generic invariant the existing
seams cannot express. Authorization and durable acceptance stay host-enforced;
a prompt cannot enforce them.

| Placement | Responsibility |
| --- | --- |
| Pure `control-bot` / `control-core` | Lifecycle, waits, counters, generation fences, resource transitions, typed effects. No goal, recipe, persona or model policy. |
| Application operations / projections | Authorization, atomic acceptance, accounting, request resolution, UI views. No second lifecycle store. |
| Tools, strategies, configuration | Schedules, recipes, decision tools, personas, learning procedures, compaction strategies. |
| Effect adapters | Persistence, clocks, model calls, containers, resource inspection. They interpret effects and revalidate authority. |

| Reference | Idea to borrow |
| --- | --- |
| [Hermes](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills) | Reusable procedural knowledge with progressive disclosure (L2). |
| [Pi Durable](https://earendil.com/posts/pi-durable/) | Small interfaces between model, storage, tools and execution; durable timers and dependency waits. |
| [tinygrad](https://docs.tinygrad.org/developer/developer/) | A small internal vocabulary with separate transformation and execution layers (an analogy, not a dependency). |
| [Kubernetes controllers](https://kubernetes.io/docs/concepts/architecture/controller/) | Desired/observed state, ownership, explicit admission. No Kubernetes-shaped object model; a controller cannot infer completion of a prose goal. |

Control stays Rust/WASM/Nix with plain files and atomic commits. Upstream
non-Rust runtimes stay inside explicitly configured tool/agent containers.
[Bot-control research](research/BOT-CONTROL.md) and
[bot inspirations](BOT-INSPIRATIONS.md) hold the longer comparisons.

## Delivery order

| Priority | Work | First exit |
| --- | --- | --- |
| P0 | [Reliability backlog](#p0-reliability-backlog): storage bounds, fault tests, provider gates, R1/R2 evaluation | Each fix ships with a fault test; comparative claims use the [paired-trial gate](#evaluation-gates). |
| Ongoing | [A1–A4 state/effect boundaries](#a1a4-remaining-stateeffect-boundaries) | Extend the established boundary with its consumers; no big-bang extraction. |
| P1 | [E1 escalation](#e1-escalation-to-the-lead) with [G1 projections](#g1-one-lifecycle-authority-narrow-projections) | One-slot parent/child resolution works through the existing question path. |
| P1 | [U1 coordination UI](#u1-coordination-ui-and-a2a-visibility) | Read existing lifecycle, requests and claims; add projections only as needed. |
| P1 | T1 terminal development workflow | Finish the gaps listed in [TERMINAL.md](TERMINAL.md). |
| P1 | [W1 schedules](#w1-cron-style-schedules) | Durable occurrence IDs and operator-only schedule writes. |
| P1, before more fan-out | [S1 service resources](#s1-service-resource-requests) and [Q1 team admission](#q1-team-admission) | Typed requests and aggregate capacity reserved before provisioning. |
| P1 | [V1 Vault](#v1-manage-vault-and-scoped-secret-injection) | Agents use secrets by opaque reference; values never reach model context. |
| P2 | [U2 interactive browser](#u2-computer-use-starting-with-an-interactive-browser); [W2 recipes](#w2-a-small-workflow-layer) | Scoped browser sessions; validated recipes over spawn/wait/message. |
| P2 | [M1 follow-ups](#m1-model-follow-ups) | Hosted API adapters; local parallel tools; multi-model review turns. |
| P2 | [L1 personas](#l1-versioned-personas-and-soulmd); [L2/L3 self-improvement](#l2l3-a-reviewed-self-improvement-loop) | Captured revisions, scoped tools, evaluation and admission. |
| P2 | [H1 layered control](#h1-layered-control) | Fewer model turns per milestone in a paired trial. |
| P2 | [I1 immune system](#i1-immune-system--antibodies-and-intel): antibodies, pathology facts, intel briefs | First slice in review; next a Google Workspace `Transport` and a paired trial of antibody notes. |
| Later | [X1 execution backends](#x1-pluggable-execution-backends), [deferred items](#deferred) | Demonstrated local need. |

## P0: reliability backlog

- **Storage bounds.** Hard writable limits for live workspaces and scratch on the
  supported macOS engine, retained checkpoint/artifact budgets and admission
  backpressure. Bind mounts and polling are not quotas. Test disk-full and
  crash behaviour without losing the last good checkpoint. (Services already
  accept an optional writable-layer `storage_limit_mb`; `/workspace` is not
  capped.)
- **Fault tests.** Inject termination around launch, checkpoint and result
  publication; auth expiry/refresh, cancellation, stale completions, duplicate
  requests, slow clients and poisoned state. Fail closed without a false
  acknowledgement or a second workspace writer.
- **Provider gates.** Claude login refresh and cancellation fault tests,
  independently of Codex. Model checks stay explicit, never in offline `doctor`.
- **Unexplained MCP stall.** A Codex attempt once exited successfully while a
  room post was still `in_progress`. The adapter now reports that as an
  interrupted attempt with unconfirmed tool outcome (`src/worker.rs`); the
  original stall is unreproduced.
- **R1/R2 evaluation.** Paired interruption/recovery comparisons and finer
  causal attribution of raw watch output. Owning a stream does not prove which
  attempt caused each line. Deliver fixes as narrow tool/adapter changes, never
  workload semantics in the supervisor.
- **Long-job health and extensions.** Long-running tools may expose a bounded
  health/progress hook (identity, generation, liveness, last observed progress).
  An alive process does not prove progress; missing metrics mean unknown. Before
  a configured limit, request more time through E1; only an authorized host
  operation changes a budget, and silence grants nothing. Test a quiet but
  healthy calculation alongside a hung process and an unanswered request.
- **Second-machine evidence.** Two independent installs complete a two-turn
  task, a restart and artifact inspection; record setup blockers.
- **Hardening.** Close the gaps recorded in the [threat model](THREAT-MODEL.md):
  malicious project code, prompt injection and resource exhaustion.

## A1–A4: remaining state/effect boundaries

The first typed operation boundary is in place; see
[state boundaries](STATE-BOUNDARIES.md) for what exists. Remaining work extends
it with its consumers. Broad `State` access is not an acceptable shortcut for
new features.

| ID | Work | Exit gate |
| --- | --- | --- |
| A1 | Unify command/approval/continuation operations across transports with narrow typed inputs. | Same authorization, lineage, rollback and checkpoint outcomes from every transport. |
| A2 | Move rendering, reads and selected persistence outside the broad lock; separate pending intent from durable completion. | Concurrent save/failure/cancel tests; durable-before-effect; benchmark under concurrent fsync load. |
| A3 | Lazy bounded transcript reads; runtime-owned journal readers and caches; visible source gaps. | Startup and resident memory scale with the working set, not full transcript hydration. |
| A4 | Narrow Host/ToolSet operations per area; extract web/TUI/MCP crates once their application interface is real. | Dispatch parity including denials and cancellation; legacy full-state access measured per slice. |

No mandatory async rewrite, global event bus, second tool catalogue or
crate-per-file split. Domain locks come only after narrow borrows and measured
contention. Plain-file storage stays; layout changes need fault tests.

## Supporting contracts

These ship with the feature that needs them, not as a foundation release.

### G1: one lifecycle authority, narrow projections

`Supervisor::status()` in `crates/bot/src/supervisor.rs` derives
`Ready | WaitingTurn | Paused | Stopping`; do not persist a second status.
E1/U1 need application projections: *needs decision* (from durable design
questions) and *waiting on dependency* (from wait/continuation records), with
generation, last transition and source references.

Keep host observations and agent progress claims as separate types. Only host
writers build observations; model input builds attributed claims. Tests prove a
claim cannot resolve a request, advance lifecycle or overwrite an observation.

### O1: ownership audit

Chat parentage is not resource ownership. First reproduce a lifetime mismatch,
or identify a feature that changes cleanup semantics, then add the smallest
retain/cascade operation. A service surviving `/cancel all` may be correct.

### Q1: team admission

Aggregate the existing per-bot `Budget` limits and model-slot admission by team,
with durable charges keyed by attempt. Spawning or redefining a bot must not
reset team charges. CLI adapters enforce turn/concurrency limits only; there is
no in-flight token or cost ceiling, and unknown usage stays visible as unknown.
Release a slot on confirmed termination; reconcile stale reservations visibly.
Add non-preempting priorities (interactive and decision work first, with aging)
only when contention shows the need. Priority never creates capacity.

### S1: service resource requests

Services already take an optional `shape` (`small` 2 GiB/1 CPU, `medium`
4 GiB/2 CPUs, `large` 8 GiB/2 CPUs) within an operator-configured node service
budget; see [persistent tools](PERSISTENT-TOOLS.md). Remaining:

- raw CPU/RAM requests and operator-defined presets;
- explicit GPU device grants (never privileged mode or arbitrary passthrough),
  with accelerator availability and utilisation in status;
- accounting that covers workers and services on the same backend;
- replaceable backends for remote capacity, with provider APIs and credentials
  kept in tools/configuration.

Approval cannot create missing RAM or a GPU. Resizing reports whether it is a
live update or a replacement and preserves data and port/grant fences. Measure
latency, throttling and memory pressure before claiming a gain from more RAM.

## V1: Manage Vault and scoped secret injection

Operators add, rotate, revoke and grant credentials in **Manage** without
pasting them into chat. Agents see only permitted metadata and use an opaque
reference. Values never enter model context, chats, tool arguments visible to
the model, transcripts, summaries, knowledge, recovery candidates or ordinary
logs, and the input flow stays outside agent-visible browser captures.

- Resolve references only in trusted effect adapters, after checking caller,
  team/chat, attempt generation, destination, operation and expiry.
- Support broker-attached API authentication and approved service bindings.
  Never expose a secret-reading tool, put keys in URLs or inject values into an
  agent's shell, environment or workspace. Code written by agents uses a
  credential-bearing broker outside its process.
- Redaction is defence in depth, not the boundary: constrain destinations and
  redirects, filter reflected responses and return bounded typed projections.
- Store values outside the checkout and application state (platform store or
  encrypted files with separately protected keys). Audit use without logging
  values. Check grants at execution so revocation reaches queued calls and
  services. A credential grants no permission to spend; consequential actions
  keep their own approvals.

**First exit:** a synthetic secret created in Manage lets an authorized agent call
a local test API; model I/O, chats, traces, logs and candidates never contain
it. Test cross-agent misuse, wrong destinations, reflected bodies, expiry,
revocation of queued calls, rotation and denied browser access. Never use real
credentials in fixtures or CI.

## E1: escalation to the lead

Extend `control_design_ask` (`src/design.rs`); do not add a second request
system. Today questions resolve through the operator. Add an agent-callable,
scoped resolution tool so a worker can route a delegated decision to its lead.
The host checks lineage and delegated authority at resolution time; naming a
resolver grants nothing. The answer path reuses the existing validated answer,
durable save/rollback, checkpoint, grant and continuation rules. Old records keep
operator resolution. Questions outside the lead's mandate go to the operator
through the existing UI/Discord path under the same identity. Access requests
and knowledge proposals keep their own flows; a design answer never grants access.

Test legacy records, the one-model-slot case, a full queue, restart,
duplicate/stale answers, reparented or cancelled work, authority revocation and
answer-save failure. Resolution must not create a second writer or deadlock
behind the child it unblocks.

Review prompts across tool descriptions, spawned agents, bot wakes, judges and
continuations: decide routine authorized steps; ask the lead when a blocker
exceeds the worker's remit; ask the operator when mandate or preference is
missing; use the permission path for access. Waiting on a healthy peer is not an
escalation. Prompt text never replaces deterministic policy tests.

## U1: coordination UI and A2A visibility

Keep the steering chat and current Rooms layout as the team's control surface.
Make ownership, current objective, progress, blocked dependency and the latest
changed fact easy to see; collapse routine activity while keeping evidence.
Show whether a message was queued, delivered or processed where receipts can
establish it. Show observation age and unknown progress. No separate dashboard
that pulls users out of the chat. Remote A2A protocol interoperability is a
separate adapter with its own tests.

Acceptance: steer an active team, follow a handoff, find one real blocker amid
retained telemetry and resume without duplicate tasks or missed direction.

## U2: computer use, starting with an interactive browser

Today Control has offline browser capture only (`control_browser_capture`; see
[demonstrations](DEMONSTRATIONS.md)). Add scoped interactive sessions:
navigation, observation and actions with visible evidence, persistent session
handles and human handoff. Drive the browser from a Rust client over a browser
protocol, taking ideas from Playwright-like tools without an authored JavaScript
layer. Bound lifetime, downloads, tabs and resources; isolate profiles; filter
private-network and metadata access unless granted. Treat page content as
untrusted and keep credentials, payments and consequential actions outside page
control. Start with read/observe tasks. Wider desktop control follows a
demonstrated need.

Acceptance: one agent builds a page, another exercises it in a browser, reports a
reproducible defect and verifies the fix in the steering conversation.

## W1: cron-style schedules

Bots already support fixed intervals (`/bot on every N`, 1–1440 minutes). W1
adds calendar schedules: team or chat, goal, cron expression and time zone, with
next-run preview, recent outcomes, pause/resume and run-now. Capture the
definition revision and a stable occurrence ID so restart never submits the same
occurrence twice. Existing slots, approvals, budgets and per-chat ordering still
govern admission.

Borrow [CronJob policies](https://kubernetes.io/docs/concepts/workloads/controllers/cron-jobs/):
Allow, Forbid (default) and a missed-run start deadline; defer Replace until
confirmed cancellation of the prior writer is tested. Run at most the latest
missed occurrence within the deadline, and show skipped occurrences with reasons.
Specify daylight-saving gaps and repeats. A tick never restarts a progressing
attempt.

Schedule writes are operator-only in the first slice: one typed operation checks
operator authority and rejects agent credentials on every transport. Agents can
propose definitions for activation. Agent-managed schedules wait until
capability restrictions survive delegation and continuation.

## W2: a small workflow layer

A declarative recipe for repeated team patterns: triggers, roles, named steps,
dependencies, bounded parallel branches, completion checks, failure handling and
result destinations, interpreted by a workflow tool over existing
spawn/message/wait operations. Recipe state lives outside the supervisor.
Choose syntax only after real recipes show what is needed. Illustrative example:

```text
Trigger: weekdays at 09:00 in the team's time zone, or run now
Lead: select one issue from the approved project scope
Builder: implement in an isolated workspace
Reviewer + tester: review the diff and run checks in parallel
On failure: return evidence to the builder, within the retry budget
On success: prepare a draft PR and post one result to the steering chat
On unresolved blocker: request a decision; resume this step after the answer
```

Validate before activation (schema, cycles, fan-out/retry limits, capabilities,
budgets, destinations) with a bounded dry run. Active instances keep their
captured revision. Retry only operations classified as safe. A recipe describes
work within existing grants; it never grants access. Behaviour-tree and
state-machine routines (see [H1](#h1-layered-control)) can be recipe steps.

First exit: a scheduled inspect/build/review recipe survives a restart and a
human decision wait, posts one outcome and leaves unrelated work running.

## M1: model follow-ups

Shared bounded model calls, local summarizers, compaction model selection and
local agent turns are described in [local models](LOCAL-MODELS.md). Remaining:

- **Hosted API adapters** behind the same model-call contract and registry,
  recording provider/model revision, limits, capabilities, usage semantics and an
  opaque credential reference. Listing a model does not establish access.
- **Local agent turns:** parallel tool execution, tokenizer-exact budgets, and a
  quality comparison against hosted models on the same tasks.
- **Summary cache retention/GC**, and summary-quality evaluation before any local
  summarizer becomes a default.
- **Model roles:** separate captured selections for worker, lead, reviewer and
  compactor.
- **Visible multi-model review turns:** named participants with explicit
  provider/model selection, isolated contexts and visible review/handoff turns in
  chat, passing bounded evidence rather than whole histories. Check availability
  before claiming independent review. Review is evidence, never merge/deploy
  permission. Make it useful for `/improve` first.

Every new attempt is a fresh provider session rebuilt from Control's committed
journal. Provider caches are optimisations, never the authority for task state.
Benchmark hardware, latency, memory and quality before choosing a runtime; do not
add a host Python/Node adapter.

## L1: versioned personas and SOUL.md

Keep four things distinct: a **lens** selects context and tool descriptions; a
**persona** describes behaviour and role; a **procedure** explains a class of
work; **policy** grants enforceable capabilities. A lens or persona never grants
access.

Add versioned persona bundles that reference lenses, optional `SOUL.md` prose
(identity, tone, collaboration preferences), procedure revisions, model-role
preferences and proposed resource needs. They are configuration resolved at
admission; the supervisor does not interpret persona prose. Personal definitions
live under the configured home roots.

Composition is deterministic and the effective context is visible: node policy;
current task, steering and standing constraints; persona; relevant procedures and
knowledge; attributed evidence. Stale persona prose cannot override newer user
direction, and tool or page content cannot promote itself to instruction.
Capture resolved revisions per attempt. Children get explicit role bindings, not
a copy of the parent's identity.

The [team advisor](LENSES.md) is the first concrete persona: the read-only judge
role plus the `advisor` lens. When bundles land it becomes a bundle referencing
that lens, with no supervisor change.

## L2/L3: a reviewed self-improvement loop

```text
task → evidence → verification → lesson proposal → review / evaluate / admit
     → versioned knowledge / procedure / lens / tool package → reuse
```

**L2: procedural learning.** Extend [knowledge](KNOWLEDGE-TOOLS.md) proposals and
review with typed procedure/lens/persona candidates: applicability,
preconditions, steps, failure modes, source attempts and test evidence, with
deduplication, supersession, expiry and rollback. An agent declaring success is
never enough for admission. Retrieval loads relevant accepted revisions only.

First exit: two agents hit the same generic setup problem; one proposes a
procedure after deterministic verification; review admits it; a fresh agent
retrieves it, succeeds with fewer redundant actions and records the revision.
Also test a stale procedure and a misleading proposal.

**L3: source and tool self-development.** `/improve` workspaces, offline builds,
read-only review, [GitHub](GITHUB.md) reads/writes and draft PRs, and
[tool packages](TOOL-PACKAGES.md) (operator import, revocation, lens opt-in) exist.
Remaining:

- **Package proposals** from agents with operator review, and a measured
  two-agent reuse demonstration.
- **Deployment/restart jobs:** capture an authorized merged revision, back up the
  installation, build, replace atomically, check `/health/build` and roll back
  on failure. The runner must be managed separately from the service it restarts
  (a separate launchd job or systemd unit). Merging stays with maintainers unless
  the operator explicitly authorizes an agent.
- **Inference/service profiles:** operator-configured immutable mount references
  and CPU/RAM limits, exposed through the same TUI/web operations.
- **Lazy task sandboxes:** acquire execution sandboxes through workspace tools with
  bounded idle leases. Codex native tools still share the provider container and
  need an adapter contract first.

Runtime registration never auto-installs agent code. Model-weight training is a
later research track.

## H1: layered control

Earlier game runs drove the system like a joystick: one small job per tool call
and a status poll, about one action per minute at two or three model round trips
each. The model should make strategic and organisational decisions; immediate
reactions belong in programs the agents write and run.

| Layer | Timescale | Runs as | Example (game) |
| --- | --- | --- | --- |
| Reflexes | 10–100 ms | always-on loop: classic control, behaviour trees, small policies | eat, flee, avoid hazards |
| Perception | 30–200 ms | small local model or parser feeding the loops | detect threats, read the HUD |
| Tactics (skills) | ~1 s | bounded routines started as watches | gather N and deliver, build a segment |
| Strategy | seconds–minutes | model turns | what next, who does it, when to change plan |

Lower layers escalate only outcomes, milestones and situations they cannot handle.

**Already available:** services and watches host the loops; the opt-in Ganglia
lens states the rule ([lenses](LENSES.md)); tool packages version reflexes and
skills; watch salience filters, declared observations (`control_observe_set`) and
score probes are described in [watches](WATCHES.md); the
[planning tools](PLANNING-TOOLS.md) give the strategy layer computed answers.

**Gaps** (build each only for a demonstrated need):

- **Accelerators:** GPU scheduling for perception/policy services (S1).
- **Summarising perception:** a model summariser over filtered events.
- **Observation feeds:** streaming frame or state input into a service for
  systems without a structured API.
- **Routines as a generic language:** behaviour trees and state machines (named
  states, guarded transitions, hysteresis, escalation as an explicit outcome) as
  W2 recipe steps. Prototype as an agent-built package run as a watch; promote a
  generic interpreter only after real routines show which constructs are needed.

First exit: agents build a shared reflex loop and skills, run them as watches and
complete milestones with markedly fewer model turns per claim than the
joystick-style baseline, in a paired trial.

## I1: immune system — antibodies and intel

People are productive because whole systems support them: immune systems,
institutions, counter-intelligence. A swarm needs the same: cheap, plentiful
observers that notice when work goes off the rails and recruit the response,
plus intelligence that tells the operator what happened. Observers stay
read-only by design; they tag and report, they never do the work or take over.

| Layer | Role | Built from |
| --- | --- | --- |
| Cortex (lead) | Strategy: goals, plans, who does what | Model turns of the steering chat |
| Ganglia (routines) | Reflexes and skills that act without deliberating | Watches, services, tool packages ([H1](#h1-layered-control)) |
| Immune (antibodies, advisors) | Patrol for pathologies; note the lead; escalate to the operator | Read-only judge bots woken by host facts |
| Intel (analysts) | Daily brief: goals, progress, cost, pathologies, open requests | Read-only summary tool, channel delivery |

**Pathology catalogue.** Host facts derived cheaply from records Control already
keeps, edge-triggered and fingerprinted: harness churn (file/tool building while
claims and scores do not advance: building a generic harness instead of placing
the stone wall), repetition loops (the same command failing again and again),
polling (status-only commands instead of a routine), cost burn (tokens since the
last advance), plus the advisor's stalls, shared blockers, bottlenecks, timeouts,
failure streaks and members that cannot checkpoint. Thresholds use
`execution.json`. Candidates for later: repeated identical posts, a backlog that
no longer matches the goal, claim churn without done, package versions without
use, score regressions.

**Escalation paths.** Facts wake the team's advisor (team scope) or an antibody
(node scope). An observer notes the lead's steering chat, which decides what
reaches the team; if the lead does not fix it, or the operator is needed, a
rate-limited operator notice goes to the operator's channels. Observers never
message members, post to rooms, claim work or start services.

**Cheap and plentiful.** An observer turn is a bounded read and at most one note,
a good fit for local models ([M1](#m1-model-follow-ups)); the host does the
counting and only wakes a model on a new fact. Many observers per node should
cost little more than one; their own turns are not team activity, so they cannot
wake themselves.

**Report sinks.** Briefs and notices go through the generic channel
`Transport` (Discord DMs today). A Google Workspace adapter (append a Docs
section, send a Gmail digest) is one more `Transport` implementation, configured
in `channels.json`, not a special case in the core.

The first slice (antibody lens, pathology facts, `control_intel_brief` /
`control.intel.brief`, `control_operator_notify`, daily channel brief) is
described in [lenses](LENSES.md#antibodies). Remaining:

- **Google Workspace sink:** a `Transport` adapter for Docs/Gmail with its own
  credential handling outside the checkout.
- **More analysts:** per-team briefs on demand from a chat; weekly trend briefs
  over the scorecard metrics (execs or turns per done claim, joystick ratio,
  operator and host-blocked time).
- **Observer evaluation:** a paired trial showing antibodies reduce wasted turns
  or tokens per milestone without noise; tune thresholds from it.
- **Local-model observers:** default antibody runs on a local model once
  [M1](#m1-model-follow-ups) tool-using local turns are reliable.

First exit: on a run that drifts into harness building or a failing loop, the
lead receives one antibody note and, when it does not correct course, the
operator receives one notice, with no repeats and no effect on healthy teams.

## X1: pluggable execution backends

Workspace jobs, services and previews assume the local Docker engine. Put
container lifecycle behind one backend interface over the existing service
`Engine` seam:

- Docker stays the default. Firecracker microVMs and remote nodes implement the
  same start/inspect/exec/attach/stop contract with generation fencing and leases.
- A host-shell backend for local development runs commands on the host with no
  isolation: explicit per-node opt-in, never a default, never selectable by a
  model, visibly marked wherever active.
- Backends declare capabilities (network modes, storage, profiles), checked at
  admission. Authorization, ownership, approvals and checkpoints stay in
  Control's domain code.

## Deferred

- **External bridge writes:** `control mcp` hides room posting and chat sends;
  expose them only with the same durable run service and no inherited
  worker/operator authority.
- **Remote transport:** scoped, revocable remote MCP or preview access (for
  example over Tailscale/SSH) when a user needs it. Private networking is not
  authentication.
- **Declarative environment requests** and a constrained image-build tool; never
  host mounts, engine options or the engine socket.
- **`control migrate`** for deliberate state upgrades once the schema settles.
- Live federation and remote delegation, bundled inference and model routing.
- Deterministic supervisor simulation, parser fuzzing, scoped capability tokens,
  artifact editing.

## Evaluation gates

Each implementation PR states its ID, the observed problem, the contract it
changes, its checks and remaining limits, and runs the relevant
[contribution checks](../CONTRIBUTING.md). Fault and cancellation tests come
before claims of recovery correctness.

Comparative claims (fewer wakes, faster recovery, fewer turns) need paired
trials. First publish a credential-free regression for the specific defect. Then
run baseline and changed revisions in fresh worlds from the same seed, steering
script, agents, models, tools, budgets and workload version, changing one factor
at a time. Verify world identity and start conditions, not just the configured
seed, and verify milestones from world or tool evidence. Predeclare the target
gain and acceptable variation; repeat trials to account for model variability.
A single successful run is a smoke test. Track milestones, useful and repeated
actions, interventions, checkpoint loss, context bytes and reported/missing
usage. Fewer wakes alone do not prove better task performance.
