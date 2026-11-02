# Bot control research and contract draft

Research only, 2026-10-31. This is a proposal, not an implemented API or a new
language commitment. No runtime changes, authenticated calls or credential reads.

## Recommendation

Ship persistent tools, scoped ports and a small cooperating team before a language.
The first bot definition should be ordinary versioned configuration: persona,
model, composed lenses, named tool resources, event subscriptions and limits.
Use a small deterministic Rust supervisor to turn admitted events into bounded
model turns and tool effects. Instructions supply goals; validated grants supply
authority. A model can propose a changed definition, but cannot authorize it.

A separate optional workflow-language project is worthwhile as an experiment
once the same effect contract can run a real bot. Its output should be ordinary
versioned event/command messages accepted through Control tools/MCP. It should
not introduce a second scheduler or become a prerequisite for gameplay.

## Fit with the current repository

`roadmap.md` already has the right sequence in “Next: persistent worker/tool
environments”: durable resource identity and leases; enforced scoped egress;
small peer teams with resource budgets separate from model slots; knowledge
admission and room notifications; pinned game tool with a Rust adapter. “Later:
personas as persistent bots” already separates instructions from enforceable
policy and explicitly defers a generic programming framework.

At the initial inspection, `crates/core/src/lib.rs` exported `scheduling` only.
The accompanying foundation branch adds pure resource transitions and optional
Serde persistence support, retaining a dependency-free default build. Preserve
this useful boundary. Pure lifecycle decisions belong here; Docker, persistence,
Discord delivery, clocks, model APIs and
network enforcement belong in adapters. A callback reducer can eventually reuse
this style without putting a parser, plugin loader or dynamic interpreter in core.

Existing delegation remains bounded parent/child model work. A persistent game
connection is a resource; a bot is a supervised identity that can request model
turns; a model attempt is one execution. These lifetimes must remain distinct.
Room worker read/post exists; the subsequent implementation adds durable inboxes
and a bounded one-shot wake. Persistent bot event subscriptions remain future
work: a persona listens across turns and restarts for selected room or tool
events, with explicit cancellation and budgets. This does not keep a model
running continuously.
Do not create competing chat or human-question systems for bots.

## What to borrow

| Source | Useful idea | Boundary for Control |
|---|---|---|
| [STC README](https://github.com/fennecs-system/stc/blob/main/README.md) | Workflow descriptions compose with `bind`, parallel branches and sequences; an interpreter schedules work separately. | Borrow program/interpreter separation. Its distributed-first Elixir/Horde model is not required for one laptop. README goals are not evidence that restart guarantees are already proven. |
| [OpenClaw hooks](https://docs.openclaw.ai/automation/hooks) | Distinguish lifecycle hooks, behavior hooks, inbound webhooks and diagnostic events. | Its internal hooks are explicitly trusted in-process JS/TS with host access. Borrow event categorization, not this execution boundary or implementation language. |
| [Grok Bot routines](https://docs.x.ai/grok-bot/skills-routines-and-automations) | Named bot ownership, separate instructions and triggers, testable routines, narrow event subscriptions, stale-data/failure policies. | Keep instructions easy to edit, but compile concrete selectors and budgets into validated data. Avoid waking a model on every game tick or room message. |
| [Grok Bot approvals](https://docs.x.ai/grok-bot/approvals-security-and-privacy) | Approval shows the concrete operation, distinguishes requester identity and external text, and separates cloud from local access. | Its automated review is model-based and documentation says it complements least privilege. A judge in Control must never replace deterministic capability checks. |
| [Meta Muse safety design](https://research.meta.ai/blog/security-and-safety-for-ai-agents-our-approach-with-muse) | A separate authority evaluates proposed actions; scoped approvals, brokered connectors and enforced egress remain outside the agent runtime. | Borrow separation of proposal from authority. Do not require its larger eBPF/taint architecture to ship the first allowlisted game connection. This is the personal-agent Muse, not Microsoft's similarly named gameplay world model. |
| [YC agent harness](https://github.com/yc-software/minecraft-agent-harness) | Persistent sessions, a configured team, DMs and task-specific tools are sufficient for a working multi-agent demonstration. | Use as evidence that a new language is unnecessary. Domain packages and agent-written adapters belong in isolated tool containers, not Control's maintained source or a host Node runtime. |

These are primary documentation claims, not independent audits of the products.
Recommendations below are design inferences for Control.

## Minimal formal contract: version 0 draft

This describes semantics, not a required Rust implementation or final wire format.
Use Rust newtypes and enums internally; Serde DTOs at transport/storage boundaries.
Derive structural traits where useful. Validated constructors, not public raw
strings or deserialization alone, establish capabilities.

### Identities and state

Distinct types: `BotId`, `BotRevision`, `OwnerId`, `TeamId`, `ResourceId`,
`Generation`, `GrantId`, `EventId`, `EffectId`, `SubscriptionId`, `Cursor` and
`BudgetReservationId`. Do not interchange actor, chat, bot and container IDs.

A bot definition names an owner, persona/lens references, model selection,
tool-resource bindings, subscriptions, concurrency and budget ceilings, and an
explicit desired state. References to existing grants request use of authority;
configuration is not itself a grant.

Runtime states: `Paused | Ready | WaitingTurn | WaitingEffect | WaitingHuman |
Stopping | Stopped | Failed`. Persist enough state to explain why a bot is waiting.
Observed resource health is separate from bot intent and model-run status.

### Event and command algebra

An event envelope has schema version, durable ID, source identity, per-source
sequence/cursor, bot revision, provenance, bounded typed payload and receipt time.
The supervisor authenticates the source and validates selectors before admission.
Applications lift these in as messages on topics: room posts, timer ticks,
resource observations, effect completions, human answers, turn completions,
grant revocations and lease expiries.

Separate external commands: `StartBot`, `PauseBot`, `ResumeBot`, `StopBot`,
`ChangeDefinition`. A room message that says “stop the bot” remains room data
unless it arrives through an authorized command route. Ownership does not arise
from a persona claiming to be an operator.

Pure transition:

```text
decide(definition, state, admitted_input, policy_snapshot, now)
    -> Result<Decision { next_state, effect_intents, consumed_cursor }, Rejection>
```

The caller supplies time and observations. No hidden clock, random draw, network,
filesystem or model call occurs in `decide`. Equal inputs produce equal decisions.
If nondeterminism is needed, record a supplied value as input. Model outputs are
recorded observations, not something replay recomputes.

Effect intents are a closed typed sum initially: `RequestTurn`, `EnsureTool`,
`CallTool`, `StopTool`, `NotifyRoom`, `AskHuman`, `ArmTimer`. Large payloads use
bounded artifact references. No generic host `Eval` or shell callback effect.

Admission and effect execution are distinct. A policy snapshot makes decisions
explainable, but the executor must revalidate current grants, expiry and generation
at the boundary immediately before the effect. A proposed effect is not permission.

### Persistence and replay

Commit consumed event/cursor, next state, budget reservations and effect intents
atomically before acknowledging the event. An outbox entry has a stable effect ID
derived from bot, revision/generation, event and effect ordinal; do not rely on
unstable language hash functions for persisted IDs.

Delivery can be at least once. Duplicate event IDs in the same subscription must
not create another decision or charge. Retain the cursor/dedup boundary durably;
do not silently prune identifiers while an upstream source can replay them.
Per-bot transitions are serialized; independent bots share bounded scheduler slots.
There is no global event-order guarantee.

Replay reconstructs decisions and reads recorded outcomes without invoking effects.
Live recovery dispatches only unfinished eligible intents. An effect that lacks a
receipt after a crash is `Unknown`, not success or failure. Retry only if its
adapter can reconcile an existing operation or guarantees idempotence with the
same effect ID. A game action such as placing a block needs observation/reconciliation
before retry; sending chat or making a purchase cannot be assumed idempotent.
Do not promise exactly-once external effects.

### Cancellation and ownership

`PauseBot` prevents new work, while an explicitly declared policy controls an
already-running turn. `StopBot` fences further effects, cancels scheduled work,
requests in-flight cancellation and stops resources owned by the bot. Both commands
have visible outcomes; stop does not assert success until cleanup is observed.

Stopping a model attempt detaches its session from a durable tool; it must not
implicitly kill a resource owned by a bot, chat or operator lease. Conversely,
an attempt cannot lend its temporary writable worktree to a persistent resource.
Sharing a resource grants attachment, not ownership: detaching one bot must not
stop another owner's service. Resource owner, authorized attachers and grant
issuer are distinct concepts.

Generation fences reject stale model results, tool observations and late human
answers after replacement/cancellation. Recheck authority independently: an old
generation result cannot mutate state or launch new work; revocation must close
existing network sessions rather than merely blocking future connects. No claim
is made that cancellation can undo effects already performed remotely.

### Budgets and event pressure

Reserve model calls/tokens or spend, resident services, CPU/memory, child/team
slots, output bytes and callback rate separately. Reconcile actual use against
reservations; include outstanding reservations across restart. Children receive
at most the parent's authority and remaining shared team budget. Independent
per-bot counters must not permit each child to spend the full team allowance.

Event subscriptions require bounded queues and an explicit overflow policy.
Coalesce replaceable state observations by resource; retain ordered human answers
and terminal outcomes. If a sequence gap matters, resynchronize or fail visibly.
Bound pending model wakes to avoid one wake per tick. Deterministic movement and
action completion stay inside the game tool; model planning is event-driven and
coarser. Timer and retry handlers use bounded backoff and a deadline; elapsed
time, health probes and retry arithmetic do not consume model tokens.

### Enforced rules versus instructions

Mechanically enforceable: source identity, tool allowlists, resource ownership,
lease expiry, generation fencing, destinations/ports, mounted path modes,
resource limits, maximum children, budget reservations and approval binding.

Instruction-only without a narrower verified adapter: “build a beautiful castle”,
“be kind”, “ask me if the design is important”, “never damage a valuable block”.
The last two can become partly enforceable if the tool exposes explicit design
approval gates and protected regions; a model's self-report is insufficient.
Formal claims apply to the modeled transition system and mediated effects, not
arbitrary generated code, prompt compliance or an external world's behavior.

## First configuration and first tests

Use the repository's chosen ordinary config format; do not add a new parser merely
for this project. A version-0 definition can express: persona “castle builder”;
five named bots inheriting one selected model; one approved resource per bot;
one room subscription restricted to team and mentions; wake on human answer,
disconnect or action completion; finite idle and total budget; ask-human route
through the existing Discord bridge. All are future configuration semantics,
not proposed CLI flags that already work.

Begin with user-triggered lifecycle requests; add room notifications only once
their durable cursor exists. Prove the value with an offline counter/echo tool
that two turns attach to, then one game connection, then five peers. No callback
language work is on the critical path.

Contract tests should cover: duplicate event gives no second effect; expired or
revoked grant denies execution; stale generation is rejected; pause/stop semantics
differ; a borrowed resource survives detachment; owner stop cascades correctly;
restart after intent commit/before receipt preserves `Unknown`; non-idempotent
effects are not blindly replayed; queue overflow is explicit; total team spend
cannot exceed reservations; a room payload cannot escalate authority. Property
tests can generate event sequences around these small pure transitions. This is
useful verifiability before choosing any proof language.

## Optional language research after gameplay

[Free monads](https://hackage.haskell.org/package/free-5.2/docs/Control-Monad-Free.html)
make a program a value interpreted by a handler (`Pure`, suspended instructions,
`foldFree`). Borrow the idea of explicit effects and alternate live/test/replay
interpreters. A small Rust enum plus pure reducer is enough now; a recursive free
monad with closure continuations complicates durable serialization. If needed
later, use named continuation/step IDs and a bounded first-order workflow AST.

[Koka](https://github.com/koka-lang/koka) is a primary example of types tracking
effects and handlers separating their implementation. Borrow an explicit effect
vocabulary, not a new runtime. A type-level permitted-effect row still needs an
actual runtime grant check for a particular actor, resource and current lease.

[STC](https://github.com/fennecs-system/stc/blob/main/README.md) offers a directly
relevant composition vocabulary: task, bind, sequence, parallel and unfold.
If repeated real workflows need it, prototype those over the stable Control tool
contract in a separate optional repository. Serializable continuations, cancellation
and replay laws are the interesting research questions; cluster distribution is
not necessary to test them. Review source and tests beyond the README before
claiming STC already solves any specific failure case.

[Current Bend](https://github.com/bendlang/bend) is Bend 2: dependent laws/proofs,
purity and affinity, distinct from original Bend/HVM2. Its README also describes
limits and proof/checker/backend trust boundaries. Interesting experiment: prove
a finite lifecycle reducer preserves authority attenuation or no-post-stop
dispatch, then test the Rust implementation against the same traces. Do not port
Control or put a Bend compiler on the gameplay path; language proof does not
automatically verify Rust adapters, Docker, DNS or game effects. The current
implementation/toolchain must separately meet Control's authored-language policy.

Lambda expressions or higher-order composition can be ergonomic in a separate
authoring layer, but persisted programs should lower to explicit serializable
nodes with limits. Do not introduce arbitrary recursion or host code execution
through configuration. An ordinary declarative bot plus a few callbacks may
remain the best design indefinitely.

Potential separate repository deliverables: a versioned schema, trace interpreter,
reference simulator, conformance fixtures and optional Rust/WASM plugin compiler
targeting Control's tools. It may propose commands; the node still authenticates,
budgets, persists and authorizes them. No in-process plugin gets ambient host
authority merely by being called a language extension.

Deferred explicitly: new syntax/compiler, general effect-row inference, arbitrary
higher-order durable workflows, automatic proof generation, cross-node consensus,
distributed work stealing, model-every-tick control, unrestricted browser/network
execution and autonomous purchases. Revisit only after a real repeated workflow
demonstrates the simpler approach's limits.
