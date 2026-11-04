# Control roadmap

Reviewed 2026-11-04 against main through #129, the continuity review and
the Cass playtest evidence; implementation and comparative retest remain pending.
Keep the normal macOS/Nix release useful to friends; improve reliability before
adding autonomous operation. Each node supplies its own configuration and model
access. Federation remains lower priority.

**Augment, don't intermediate.** Control supplies tools, isolated environments,
knowledge and review around a person's model access. Web, TUI, Discord and MCP
are interfaces to shared capabilities. Tool availability, resource grants and
transport are separate. Lenses are pure, composable context/tool selections;
personas describe behavior and policy, not additional authority.

For one owner, sustained high fan-out may hit subscription limits; prefer API keys
for 50-agent runs. Control roles do not change credential ownership. Operators
remain responsible for provider terms; using Control to share account access or
otherwise misuse a subscription risks violating those terms.

A ✓ in a title means the described implementation exists. It does not close
unperformed release checks. Branch-only work is identified separately.

## ✓ Implemented on main

### ✓ Private configuration and setup

- Home-directory configuration/state roots, explicit overrides and precedence;
  no author-specific credentials or Discord identities in source defaults.
- Numeric Discord ID bindings with separate user/architect roles, DM-only
  ingestion, recipient checks and node-level task authorization across providers.
- Embedded UI/security assets, repeatable Nix setup, offline `doctor`, a short
  README and setup documentation. Friends build the ordinary release from main.
- `control open` opens the default browser; `control open --link` prints a private,
  single-use link for another local browser.

### ✓ Stateful chats and basic harness interaction

- Published workspace checkpoints between turns, separate Control improvement
  snapshots, cancellation, graceful shutdown and restart reconciliation.
- Attempt fencing, ordered per-chat work, explicit retry, retained diagnostics
  and progress events without a full durable save for every event.
- `/steer` queues next-turn direction; `/btw` and `/spawn` create linked independent
  conversations. Bounded model slots and at most 32 pending runs bound admission.
- Web/TUI command activity and purple/red diff previews; bounded-context omission
  is visible. Artifact explorer and read-only text viewer share MCP list/read.
- Structured design questions with options or free-form steering; actor-bound
  answers and paused checkpoints survive restart across web, TUI and Discord.

### ✓ Claude and Codex integration

- Per-run saved backend/model selection, pinned CLI adapters, supported login,
  bounded stream parsing and provider-reported usage/cache accounting.
- Claude executes commands through separate offline, credential-free workspace
  containers. Built-in harness tools stay disabled.
- Fresh Claude setup selects `sonnet`; saved model rows select the default.
  Browser chat refresh and authorized improvement-workspace setup fixes are merged.
- External clients can use `control mcp` independently of managed CLI runs.
  Its advertised surface remains narrower than the full node interface.

### ✓ Core context and collaboration surfaces

- Composable lens library, linked rooms and worker room tools, knowledge notes
  and graph, searchable tool catalogue and registration checks.
- Public web search, offline Rust playground and offline browser capture.
  Browser capture is not an interactive internet browser.
- Local Rust tests/build checks in CI and documented local contribution checks.

## ✓ Approved project workspaces and local previews

- `/open`, TUI `--directory` and a compact web folder control request node-local
  access; only explicit operator confirmation imports files. Agents have a
  request tool, never an approval tool.
- Independent private Git worktrees preserve current files and history while
  leaving the original checkout unchanged. Full project checkpoints are separate
  from bounded artifact previews; successful matching attempts advance the head.
- Local preview start/stop tools run credential-free, offline services against a
  read-only project mount. Loopback ingress uses a separate browser origin;
  durable identity is verified before restart rebinding or cleanup.
- Lite workers retain read-only filesystems and Rust tools. Opt-in full workers
  add Nix and common runtimes, with a private writable layer for credential-free
  tools; model CLI containers stay read-only. See [worker profiles](docs/WORKER-PROFILES.md).

See [projects and previews](docs/WORKSPACES.md) for limits and tested boundaries.
Native file pickers, in-place host edits, Jujutsu and remote publishing are not
part of this implementation. This work is merged into main; deployment remains
a separate maintainer action.

## ✓ Self-development tools

- Folder approvals follow their chat request, with inline errors and replaceable
  pending paths. Multiple approved folders compose under `/workspace/<name>`;
  imports skip symlinks and preserve each folder's isolated Git metadata and edits.
- Durable long-running command jobs stream bounded output, support cancellation
  and deadlines, and fence checkpoint publication until writers stop. Restart
  reconciliation verifies immutable container identity.
- An optional declared Rust/WASM build image vendors the pinned dependencies for
  offline tests and builds, without credentials, host sockets or shared writable
  caches. See [build environments](docs/BUILD-ENVIRONMENTS.md).
- Workers can delegate bounded child tasks and resume with retained results.
  Workers and judges inherit the captured parent model; judges have enforced
  read-only source. Explicit cross-model review needs an availability-aware flow.
  Child changes require explicit acceptance; execution uses bounded node slots.
  See [delegation](docs/DELEGATION.md).
- A repository-bound GitHub broker publishes completed improvements as draft
  PRs with captured source provenance and retry reconciliation. Live publication
  needs the operator's private credential-file reference. See [GitHub](docs/GITHUB.md).
- The [threat model](docs/THREAT-MODEL.md) links enforced boundaries to tests and
  distinguishes guidance from policy. Hard disk quotas remain open.

## Demonstration goals

The basic generated-page exercise is now documented in
[demonstration goal 1](docs/DEMONSTRATIONS.md): save/build a Rust-served page,
start a preview, capture self-contained HTML in offline Firefox, inspect and
revise it. The new worker capture was verified with a real Nix Firefox image.
This is the first usability exercise; the full persisted-data website and
interactive browser scenarios below still need end-to-end evidence.

Use [four concrete demonstrations](docs/DEMONSTRATIONS.md) to test Control's
breadth, after the self-development build/test loop. These are planned goals,
not completed features or additional special cases in the core.

1. **Non-trivial website:** build, test, serve and revise a useful multi-view
   website with persisted data through Control.
2. **Browser research and a tool view:** agents collect comparable phone prices
   from different sites; a Rust/WASM tool view in Control shows observations over
   time, evidence, missing data and failures.
3. **Minecraft team:** start an offline-mode server in a tool container, expose
   its approved port, then have five configurable agents join using Mineflayer
   inside their own tool containers. Agents can write and share tools, coordinate
   through rooms and share admitted knowledge. Connections outlive model runs.
4. **Play and steer:** the owner joins the same Minecraft world, asks the team
   to build a castle and answers a design question through Discord, such as
   stone brick versus cobblestone. The answer steers the same task.

For multi-agent demonstrations, prioritize persistent tool lifetimes → scoped
ports → a small team using its own tool runtime, with shared knowledge and room
notifications for coordination. Interactive browsing is a separate demonstration.
Storage, recovery and hardening checks below gate the corresponding
work. Website and research demonstrations can progress independently.

Non-Rust third-party tooling is allowed only inside tool/agent containers.
Control and maintained integrations remain Rust/Haskell/Lean/WASM/Nix.
Agents may write task-specific programs in other languages inside granted tool
containers. Their server packages, adapters and configuration stay outside this
repository; Control provides generic resources, ports and tool contracts.

## ✓ Performance pass

`performance/rust-hot-paths` borrows typed transcript data, scopes run indexes to
the requested conversation, and avoids discarded log allocations during ownership
snapshots. A bounded offline Rust microbenchmark tool is exposed through MCP.
See [measurements and reproduction](docs/PERFORMANCE.md). Execution policy is
unchanged; this does not add parallel model runs.

## ✓ Bounded concurrent model execution

Two model slots by default, configurable from 1–8 through private node settings.
Independent chats overlap; each chat retains FIFO ordering and one writer.
Dispatch is durable before thread launch, supervised and joined on shutdown.
Tests cover ordering, cancellation, shutdown, save rollback across chats and
untrusted saved settings. See [execution](docs/EXECUTION.md).

## ✓ Shared libraries and measured delivery projections

Rust 2024 is used by application packages, WASM and standalone Rust tools.
`control-ai` shares bounded Codex/Claude record decoding, normalized activity and
usage without application state or credentials. MCP schemas and typed transport
metadata are shared across discovery and dispatch. Discord is compiled once as
a library adapter and retains typed delivery fields rather than a full JSON tree.
The synthetic delivery-reader comparison measured about 44% less decode time and
55% lower peak process RSS; this is not an end-to-end latency claim.
Web folder access is opt-in through `/open`, with separate approved named folders.
See [library boundaries and measurements](docs/CRATE-BOUNDARIES.md).

## ✓ Supervised bots, watches and conversation journals

- Pure `control-bot` state/event/effect transitions, durable room/watch/timer
  callbacks, operator-approved bot requests and `/bot on|pause|resume|off`.
  Watches run in persistent services and report output across model turns.
- `control-comonad` supplies Store/zipper and lens primitives. The shared
  conversation interface supports Recent, Passthrough and Lens compactors.
  Background summaries are still planned; the primitives alone do not preserve
  all older context in every prompt.
- Append-only journals retain completed activity and run text; typed outbox
  events separate frontend delivery from application state (#118–#124).
  Run text is still hydrated at startup and messages remain in state. This
  reduces checkpoint size without claiming bounded resident history.
- Watches wake by default, services expose their platform, and full images carry
  archive tools (#126). Wake guidance encourages owned next steps rather than
  repeated permission questions (#127); redundant room skill installation is
  removed and service output is folded in chat (#128).

## Baseline: deployed revision and turn continuity checks

**✓ Merged in #129 with local verification; deployment status unverified here.**
Automatic turns retain omitted user instructions and recent execution evidence
through the shared provider-neutral projection. Explicit one-shot watch requests
re-arm the next callback, completed/pending evidence survives new watches, and
failed saves restore observation counters. `control_bot_pause` durably pauses
the caller's own wakes; the operator resumes it with `/bot resume`.

Record whether the playtest binary/WASM and worker images include #129 and check
node health before comparative testing. Verify that the original goal and later
steering survive callbacks, setup advances without manual "continue", and room
updates reflect actual progress. Waiting on a peer should end a turn quietly;
an explicit pause should stop automatic wakes. Inspect wake reasons and tool
evidence if progress stops. Fresh provider processes alone are not a defect;
the context and callback contract is what must hold.

Local workspace/web checks, Chrome's fifteen-turn regression, release builds
and before/after flamegraphs pass. Prompt construction adds measured work for
retained evidence; see [performance results](docs/PERFORMANCE.md). The workload's
connection/protocol work belongs in tool containers and must be distinguished
from the stop/start overhead measured below. Keep broader extraction out of the
continuity implementation's path.

## Next priority: reduce stop/start context pollution

**Implementation in progress; not deployed.** The Cass playtest
shows 17 consecutive monitoring turns in ten minutes, repeating 68 tool calls
without a gameplay action. Their exchanges occupy about 52% of the reconstructed
next recent-history window. See the scoped measurements, concrete contracts,
code map and acceptance tests in [Turn continuity implementation handoff](docs/TURN-CONTINUITY.md).
Keep the current playtest unchanged as baseline evidence.

Bot = agent + supervisor + callbacks. Keep a progressing session active; resume
unfinished runnable work automatically when that session ends. Do not impose a
short periodic handoff target. The feature branch has bounded captured input,
typed working notes and conversation/room receipts, atomic publication, idle
waits and budgeted successors. Bot approval enables the next admission without
restarting an active session. A missing handoff can recover from a successful
published checkpoint; failures and approval waits retain their separate paths.
See the design's implementation checkpoint for the supported subset. Watch
receipt expansion, evidence/resource resolvers and prefix/exit wake selection
remain follow-ups, not prerequisites for this continuous-agent playtest.

Control's JSON state, journals and resource/checkpoint records remain the source
of truth. Separate retained evidence, explicit working state and the bounded
context projected into a model turn. Keep the continuation contract independent
of provider sessions, with the same meaning for Codex, Claude and later API-key
or local-model adapters. Preserve the existing scheduling and authority rules.

1. **Useful task context.** Preserve lifecycle evidence for inspection, but keep
   repeated wake/status bookkeeping out of managed task history. Capture exact
   Control input, projection version and evidence selection. Preserve standing
   instructions across trigger types and bound the whole context projection.
2. **Reliable working-state handoffs.** Version goals/progress, evidence-linked
   findings, rejected approaches, pending actions/handles and conditional next
   steps. Stage model claims separately from Control-observed facts; atomically
   commit handoff, processed-event cursors and matching workspace publication.
3. **Active sessions and event deltas.** Allow several related actions in
   one attempt, consuming deltas at action boundaries. Yield on genuine waits or
   budgets; retain at most one durable successor for unfinished runnable work.
   Events already processed by that attempt must not create redundant wakes.
4. **Selective admission.** Separate recorded telemetry from wake eligibility.
   Use generic watch milestone/exit policies and scoped wait conditions, preserving
   unread evidence, finite rechecks and human/failure delivery. No game-specific
   logic belongs in Control. Idle agents release their model slots.
5. **Preserve recovery and measure progress.** Keep one writer, durable admission,
   generation fencing, cancellation, pause and delegation chains. Restart still
   interrupts attempts; never replay ambiguous effects. Compare useful actions,
   model admissions, monitoring turns, context bytes, token usage and task time
   on equivalent tasks explicitly permitting longer slices. Cached comonad
   summaries and API/local-model adapters remain separate later workstreams.

## Later: core, web and terminal boundaries

Build on the persistent-tool lifecycle/ownership rules already extracted below.
Extract narrow application operations where they remove coupling; keep usable
tools and demonstrations ahead of the complete web/TUI split.
The existing server still combines mutable application state, routing, rendering
and execution assembly. `control-core` contains the first pure rules; the full
application service and separate `control-tui`/`control-web` adapters remain
incomplete. Keep `main` buildable throughout small reviewed slices.

1. Establish typed application commands and bounded queries used by web handlers
   and MCP. Keep authorization, durable acceptance, cancellation and stale-result
   fences behind that interface. Preserve saved-state compatibility and ordering.
2. Extract `control-core` for conversation/run transitions, scheduler admission,
   resource ownership and supervision contracts. Provider decoders remain in
   `control-ai`; concrete tool/container executors implement effects. Core must
   not depend on HTTP, HTML, terminal rendering or Discord.
3. Extract the existing MCP client and terminal presentation into `control-tui`,
   removing duplicated source inclusion. Extract `control-web` for web routing,
   rendering and assets over the same application interface. Browser WASM remains
   a client; in-process web calls need not serialize through loopback HTTP.
4. Move rendering outside the mutable-state lock using bounded projections, then
   add conditional transcript updates with complete invalidation. Measure fixed
   render/lock/allocation fixtures and targeted rebuilds before/after. Retain a
   change only when it preserves behavior and has a useful boundary or measured
   efficiency gain; do not claim moving files alone makes execution faster.

Implemented slices: `control-core` owns pure scheduling, attempt-generation
predicates and typed resource transitions, with independent tests and a
dependency-free default build. Persistence uses optional validated Serde support.
The application adapter retains config I/O and the persisted runtime. Compatibility
checks compare it with the previous scheduler over mixed saved states. This does
not yet extract the full command/query service, web adapter or terminal client.
MCP should become a protocol adapter over that service, preserving today's shared
tool catalogue rather than introducing a separate tool implementation.

Do not expose the full state mutex as the interface, clone complete histories,
introduce a distributed framework or make every tool its own crate. This improves
the seam for later remote execution; remote authentication, resource ownership
and stale-execution fences still need their own implementation and tests.

## ✓ Persistent offline tool foundation

Merged and locally deployed in #33: chat/actor/node ownership, typed lifecycle
decisions in `control-core`, durable start/status/attach/stop handles, captured
immutable Nix images, private tmpfs, resource limits and explicit lifetimes. A trusted
Rust watchdog enforces expiry while Control is offline. Model turns attach to a
resource rather than owning its lifetime. Live restart and missing-receipt tests
passed. See [persistent tools](docs/PERSISTENT-TOOLS.md).

Subsequent work uses private disk-backed service storage with tmpfs scratch;
until-stopped services retain files across model turns.

Resource-bound commands now provide private file/setup access after service start
and port approval. Durable receipts prevent retry replay; timed-out commands
are reaped independently, stopping the service only when that cannot be verified
(#116). Final logs survive short-lived service removal. Two-service
routes reuse separately approved ingress and loopback egress rather than a new
container network or host mount capability.

Keep bounded slots, per-chat FIFO ordering, isolated workspaces and captured
provider/model selection. Preserve exact resource ownership, cancellation cascades,
stale-result fences and restart reconciliation as service lifetimes expand.
Add aggregate node resource budgets before increasing fan-out further;
deterministic Rust supervision remains responsible for scheduling.

### Shared tool prerequisites

1. **✓ Offline lifecycle.** Merged/deployed as above. No service may retain a
   writable attempt mount across its checkpoint barrier. Tmpfs is not durable
   disk, and attachment currently reads metadata/logs rather than exposing exec.
2. **✓ Scoped TCP ports.** Operator-approved ingress and pinned-destination egress
   bind to a verified service, owner, node, generation and approved lifetime. Containers
   remain on `--network=none`; trusted relays close on revocation, expiry or loss
   of their host connection. Raw ingress rejects browser HTTP/TLS before sending
   client bytes to the service; websites use the guarded preview tool. See
   [port tools and limits](docs/PORTS.md). HTTP page grants remain separate.
3. **A small team, then scale.** Start with a few persistent tool connections.
   Keep service/resource budgets separate from concurrent model-turn slots.
   Nonblocking task batches are implemented below; they are not persistent bot
   identities or tool connections. New batches queue deduplicated completion
   responses without reserving the parent chat. Explicit delegation still awaits
   its dependency. Chat-bound bots now subscribe to rooms, watches and timers.
   Preserve the bounded
   scheduler and measure resource use before increasing concurrency.
4. **Shared evidence and coordination.** Knowledge admission and durable room
   inboxes/one-shot follow-ups are implemented below.
   Persistent bot subscriptions and wake budgets are implemented; reuse existing
   human questions and Discord delivery for steering.
5. **Tool construction and first demonstration.** Agents request/select a pinned
   tool runtime, write their own task-specific code and coordinate through normal
   tools and rooms. Operator-selected lite/full images provide an initial runtime
   choice. Agent-built images and reusable persona definitions need their own contracts;
   generic ports alone do not supply an arbitrary runtime image.
   Keep domain-specific adapters and server setup outside Control's source.

Keep these as small, usable increments. Measure lifecycle/attach overhead and
bounded status projections alongside existing command-job behavior; do not claim
an extraction improves performance without evidence. Full web/TUI extraction can
continue where it reduces coupling, without delaying usable tool workloads.

**✓ Service-lifetime ports and concurrent relays.** Services can use an explicit
until-stopped lifetime; port grants can follow that lifetime and remain revocable.
Relays retain listeners across reconnects and queue excess connections. The old
one-hour/one-connection limitation is superseded. Resource and transport limits
remain separate from model-turn slots; see [ports](docs/PORTS.md).

## ✓ Scoped web and collective tools

Merged in #34 after local checks. Rebuild the pinned worker/browser images before
using the new tools.
Existing shared tool discovery, authority checks and scheduler remain
the entry points. No authored JavaScript/Python or new orchestration framework.

- **Scoped HTTP:** operator-approved exact origins per chat/actor with expiry,
  DNS pinning, public-address checks, redirect revalidation and revocation gates.
  GET-only bounded text/evidence; ordinary workers/services stay offline.
  [Web access](docs/WEB-ACCESS.md) describes in-flight request limits and why
  this is not TCP egress or authenticated/interactive browsing.
- **Worker browser evidence:** self-contained HTML capture in offline Nix Firefox,
  bounded PNG artifacts and inline model images. It checks supplied HTML, not a
  running preview's behavior or remote assets. Preview navigation/interaction
  remains a separate scoped browser capability.
- **Knowledge:** captured-document search/read, shared admitted claims and concise
  idempotent proposals with provenance. The operator admits proposals through
  Knowledge Review; agents cannot rewrite accepted facts. [Contract](docs/KNOWLEDGE-TOOLS.md).
- **Room notifications:** durable scoped inbox cursors, ordered paged reads,
  explicit acknowledgement and an optional one-shot follow-up per human task.
  Owner-enabled `/listen` supports ongoing human/agent replies without a lifetime
  reply quota; cooldown, bounded batches and the regular queue limit concurrency.
  Restart-safe follow-ups retain team lineage. Watches survive authorized delegation and
  design-question continuations. [Contract](docs/ROOM-NOTIFICATIONS.md).
- **Nonblocking N-agent tasks:** durable batches reserve a configurable root
  budget, with a separate node backlog limit and existing model-slot/32-pending-run
  limits. Defaults are 64 agents per root and 256 pending intents. Admission
  materializes children as capacity frees; status is paged, cancellation fences
  backlog and descendants, and retries/restarts retain exact receipts. A 50-agent
  admission/restart test passes without model calls. [Contract](docs/DELEGATION.md).

Next usability checks: run the basic page task through a real model, then a
meaningful revision/reconnect. Child design questions and descendant approvals
are surfaced in the parent chat (#107); actor-bound Discord delivery remains.
A parent agent cannot grant itself additional access.

## Next: reliability and real-user feedback

Finish these before increasing fan-out further, unattended operation or spending.

Review triage, after the next small playtest:

1. **Visible history-read failures (new):** distinguish unavailable/corrupt
   activity from an empty record in UI and model context. Preserve the cause;
   test missing entries and decode failures without hiding retained evidence.
2. **Bounded HTTP admission (new):** limit concurrent request handlers and reject
   excess work predictably. Request deadlines alone do not bound thread count;
   verify slow-client saturation and recovery without disrupting workers.
3. **Shorter state-lock scope (existing extraction work):** move expensive
   projection/journal reads and eventually checkpoint I/O behind explicit runtime
   effects, using narrow immutable inputs and revision fencing. Keep atomicity
   and rollback guarantees; measure lock hold time before adding domain locks.
4. **Lazy history and explicit persistence (existing S1/S2 follow-up):** measure
   startup hydration, whole-record decoding and journal-cache eviction. Prefer
   bounded observation reads and explicit checkpoint projections; runtime-owned
   journal services can replace global/thread-local machinery incrementally.
   Background summaries remain the separate context-budget task below.

- **Storage bounds:** enforce hard writable workspace/scratch limits on the
  supported macOS engine, plus retained checkpoint/artifact budgets and admission
  backpressure. Bind mounts and polling are not quotas. Test disk-full and crash
  behavior without losing the previous good checkpoint.
- **Recovery:** persist exact daemon container IDs around worker launch, including
  the creation-to-recording crash window. Reconcile node/attempt/image/mounts;
  ambiguous resources fence work rather than being adopted or broadly deleted.
  Keep preview and worker resource handling consistent.
- **Fault tests:** inject termination around launch, checkpoint and result
  publication; auth expiry/refresh, cancellation, stale completions, duplicate
  requests, slow clients and poisoned state. Preserve diagnostics and fail closed
  without a false acknowledgement or a second workspace writer.
- **Provider verification:** complete Claude refresh and cancellation fault gates;
  demonstrate its credential/PID and discovery boundaries independently of Codex.
  Keep model checks explicit rather than calling providers from offline doctor.
- **Second-machine evidence:** record verified engines and two independent friend
  installs with a two-turn task, restart and artifact inspection. Recheck default
  model availability for their accounts; record useful feedback and setup blockers.
- **Hardening:** maintain the threat model and close its recorded gaps, including
  malicious project code, prompt injection and resource exhaustion. Read-only
  images alone are insufficient. Keep credentials, host sockets and policy
  outside agent reach.

## Next: consistent tool capabilities

The code-quality fixes preserve document literals, fence failed preview launches,
retain UTF-8 report prefixes, narrow rollback snapshots and share bounded private
reads/atomic writes. Preview proxies use stop flags rather than polling state.
The shared HTTP response crate and chat command parser are implemented.
Conditional transcript rendering still needs complete invalidation.

- Shared MCP schemas and typed transport/checkpoint metadata drive HTTP discovery,
  stdio filtering, native-client timeouts and broker dispatch. Worker discovery
  reuses the schema builders; per-run room, backend, judge and authority filters
  remain explicit. Registration tests cover policy, schema and UI completeness.
- Knowledge search/read and bounded proposals are implemented above. Add
  authorized room posting in the external bridge and
  `control.run.submit/status/diff` using the
  same durable run service. External callers inherit no worker/operator authority.
- Expose useful bounded preview diagnostics; keep service identity and ingress
  transport separate. Add scoped, revocable remote MCP or preview transport over
  Tailscale/SSH when a real user needs it. Private networking is not authentication.
- Provide declarative Nix environment/tool requests. Use pinned inputs and private
  writable stores/profiles/build state, with bounded resources. Agents can use
  advertised environment tools freely within their grant; package fetching is an
  explicit network capability. Never expose the host daemon or engine socket.
  Start with operator-selected lite/full workers. A later image-build tool must
  accept constrained build inputs, never arbitrary host mounts, engine options
  or authority; it is not required for the first small playtest.
- Keep shared images immutable. Cache reads and publication are separate policies;
  never promote a worker's filesystem or generated output into a shared base/cache
  automatically. Defer caching complexity until measured reuse justifies it.
- Add API/self-hosted adapters through the same bounded executor contracts. Personal
  subscription use and node authority remain separate; operators must comply with
  provider terms. Authorized API-backed paths with budgets are the unattended path.

## Next: visible multi-model review turns

Keep delegation on the captured parent model until explicit model selection,
availability/authentication checks and actionable failure recovery are present.
Being saved in the model library does not establish a working login.

Expose the existing author/judge pattern in chat: named participants with explicit
provider/model selection, isolated contexts, and visible review/handoff turns.
Pass selected task evidence and bounded feedback between participants, rather than
copying every participant's conversation history. Allow the user to steer the
discussion and accept an outcome. Reuse durable delegation and authority ceilings;
review evidence does not grant merge/deploy permission. Make this useful for
`/improve` before adding a general autonomous discussion system.

Prefer small typed lifecycle states, validated identifiers and explicit
state/event/effect transitions with independent tests. Verify narrow invariants
where useful; do not claim types can prove arbitrary model behavior. Keep single
node reliability and the tool/bot demonstrations ahead of distributed
frameworks or broad rewrites.

## Later: reusable personas and richer bot policies

[Bot-control research and the version-0 contract draft](docs/research/BOT-CONTROL.md)
recommend ordinary versioned configuration, pure transitions and typed effects
first. Callback/language experiments belong in an optional separate project over
the tool contract; they do not block the five-bot gameplay milestone.

A bot is a supervised instance using a persona, tools, state and explicit grants,
not a second agent runtime. Preserve the original personality + policy +
declarative-directive model; keep its configuration distinct from its chat history.

- **Persona definitions:** declarative personality, behavior directives, lens
  composition, model selection and limits. Validate structure and enforceable
  policy before launch. Clearly separate mechanically enforced constraints from
  natural-language preferences; do not claim to verify arbitrary model behavior.
- **Richer callback definitions:** extend the implemented room/watch/timer
  supervisor with declarative workflow definitions only when a concrete task
  needs them. Reuse its typed events, scheduling, deduplication and cancellation;
  a model decides where judgment is needed. Retry/deadline policies should remain
  explicit. No generic programming framework is required for the next playtest.
- **Bot spawning:** give bots a tool to request children with bounded depth,
  concurrency, token/spend/storage budgets and inherited authority ceilings.
  A child cannot grant itself or its parent more access. Existing human `/spawn`
  and bounded worker-to-worker delegation are implemented, with bounded parallel
  model runs. Independent service lifetimes are implemented; nonblocking task
  batches and operator-approved chat-bound bots are implemented above. Reusable
  versioned persona definitions and richer resource/spend policies remain planned.
- **Human questions:** reuse durable design/approval requests and Discord delivery,
  with recipient/run identity, expiry and restart-safe answers. Important missing
  information can reach the operator away from the browser; channel messages or
  callback payloads never grant architect authority.
- **Persistent judges:** independently selected author/reviewer models, bounded
  evidence and explicit acceptance criteria. Limit reviews and fan-out; use
  authorized API/self-hosted access for unattended work. A review is evidence,
  not permission to merge, deploy or purchase.
- **First workflow:** watch an explicitly selected release/source, propose a patch
  in an approved isolated project, run deterministic checks, request independent
  review and notify the owner. Merge/deploy remains an explicit grant.
- **Later external actions:** a travel watcher could report a Qantas sale and
  prepare a booking proposal. Buying requires explicit, scoped authorization for
  the concrete itinerary, passenger, price/budget and action; broad chat intent,
  website text or persona instructions are not payment authority. Retain effect
  intent, receipts and ambiguous-outcome handling; do not blindly retry purchases.

## Later: interactive browser as a tool

Extend capture into a sandboxed browser session only when the execution and
permission boundaries are ready. Keep the session lifecycle, browser profile,
network grant and artifacts explicit; tools/lenses select it without granting
accounts or host access.

- Support navigation, observation and actions through a narrow browser harness;
  code-driven automation can use a Rust client over a suitable browser protocol.
  Take interaction ideas from Playwright-like tools without introducing an
  authored JavaScript/Node application layer.
- Bound session lifetime, downloads, output, tabs and resource use. Keep profiles
  and downloads isolated, base images immutable, and any accounts scoped to the
  task. Filter private-network/metadata access unless explicitly granted.
- Treat pages, scripts, downloads and their instructions as untrusted. Keep
  consequential actions, credentials and payment confirmation outside page control.
  Start with reproducible read/observe tasks before logged-in transactions.

The [OpenClaw, Grok Bot and Muse review](docs/BOT-INSPIRATIONS.md) records the
primary sources and useful contracts. Borrow those contracts rather than their
runtimes or an entire feature list.

## Later: pluggable execution backends

Workspace jobs, services and previews currently assume the local Docker engine.
Put container lifecycle behind one execution-backend interface, building on the
existing service `Engine` seam and the typed tool host, so the same domain rules
can drive different isolation levels.

- Docker remains the default. Firecracker microVMs and remote nodes become
  backends with the same start, inspect, exec, attach and stop contract,
  generation fencing and lease expiry.
- A host-shell "yolo" backend runs commands directly on the host for local
  development and for exercising service, job and port flows without building
  worker images. It provides no isolation: explicit opt-in per node, never a
  default, never selectable by a model, and visibly marked wherever it is active.
- Backends interpret effects; authorization, ownership, approvals and
  checkpoints stay in Control's domain code. Capability differences (network
  modes, storage, profiles) are declared by the backend and checked at admission.

## Later: unbounded conversation memory as a comonad

The merged #129 continuity fix and the prioritized active-slice/handoff design are
described above. Background summaries follow that implementation's evidence: retained human
inputs can grow and excerpts still hydrate the previous journal record. The
current production Lens compactor uses `NoSummaries`; older multi-entry blocks
are omitted, not backed by a computed summary cache.

Persistent bots keep conversations that never end, so their memory needs bounded
per-turn context over unbounded history. Borrow the summary tree from
[UniiChat](https://github.com/VictorTaelin/UniiChat), exposed as a plugin rather
than built into the core.

- A conversation is a comonad: a Store/zipper focused on its current end.
  `extract` is the latest message; `extend` gives every position its own view,
  detailed near the focus and coarser with distance; the summary tree is a
  lazily materialized `duplicate`. Zooming opens a node into the two it merged,
  down to the original message, as a lens.
- Each message gets a one-line summary and neighbouring summaries merge pairwise
  into a binary tree. A turn's context is a cut through that tree within a byte
  budget. Cache summaries against immutable source blocks and summarizer versions;
  retain provenance and access to the original evidence. Measure provider prompt
  cache reuse separately; stable blocks do not guarantee cache hits or semantic
  preservation. This bounds the history view over growing retained history.
- Combine distance-based detail with explicit standing constraints and unresolved
  work from the continuation state. Old pending actions may matter more than
  recent routine output. Budget the whole context projection, allow focused
  expansion by reference, and keep summaries distinct from authoritative facts.
- Verbatim messages and summary nodes live in per-chat append-only JSON-lines
  files, not a database and not the main state checkpoint. This also moves
  transcripts out of `state.json`.
- The tree, cut and zoom are pure; summarizing a message or merging two nodes are
  effects that a cheaper model interprets in the background, within a budget and
  off the turn's critical path. Agents get a zoom tool; the plugin supplies the
  context view through the typed tool host, so other memory strategies can
  replace it.

## Deferred

`control migrate` for deliberate state upgrades after the schema settles.
Development uses explicit fresh-state resets, never an automatic wipe of an
active node or a migration prerequisite for bot continuity.

Live federation and remote delegation, other host platforms/NixOS packaging, GPU/microVM
tiers, bundled inference and model routing follow demonstrated local needs.
Keep the pure Rust context core and offline federation boundary.

Broader work can include a typed run state machine, deterministic supervisor
simulation, parser fuzzing, scoped capability tokens/session expiry, a transactional
store when JSON costs warrant it, artifact editing/highlighting and removal of
remaining simulated UI. A Nix `packages.control` derivation is still future work;
Nix setup and image builds already exist.

Every step should leave something useful to try. Keep tools and UI concise,
knowledge admission selective, and routine scheduling/authorization/retries out
of model token budgets. Measure before adding infrastructure.
