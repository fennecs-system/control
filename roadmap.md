# Control roadmap

Reviewed 2026-10-30 against the implementation and feedback from the first users.
Keep the normal macOS/Nix release useful to friends; improve reliability before
adding autonomous operation. Each node supplies its own configuration and model
access. Federation remains lower priority.

**Augment, don't intermediate.** Control supplies tools, isolated environments,
knowledge and review around a person's model access. Web, TUI, Discord and MCP
are interfaces to shared capabilities. Tool availability, resource grants and
transport are separate. Lenses are pure, composable context/tool selections;
personas describe behavior and policy, not additional authority.

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
  conversations. One execution slot and at most 32 pending runs bound admission.
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
- Base filesystems remain read-only. Agents receive factual environment guidance:
  current images are Nix-built but do not include the `nix` command.

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
  A judge uses a configured different provider and enforced read-only source.
  Child changes require explicit acceptance; execution uses bounded node slots.
  See [delegation](docs/DELEGATION.md).
- A repository-bound GitHub broker publishes completed improvements as draft
  PRs with captured source provenance and retry reconciliation. Live publication
  needs the operator's private credential-file reference. See [GitHub](docs/GITHUB.md).
- The [threat model](docs/THREAT-MODEL.md) links enforced boundaries to tests and
  distinguishes guidance from policy. Hard disk quotas remain open.

## Demonstration goals

Use [four concrete demonstrations](docs/DEMONSTRATIONS.md) to test Control's
breadth, after the self-development build/test loop. These are planned goals,
not completed features or additional special cases in the core.

1. **Non-trivial website:** build, test, serve and revise a useful multi-view
   website with persisted data through Control.
2. **Browser research and a tool view:** agents collect comparable phone prices
   from different sites; a Rust/WASM tool view in Control shows observations over
   time, evidence, missing data and failures.
3. **Minecraft collective:** five configurable Sol-backed agents join one world,
   coordinate through chat, share admitted knowledge and versioned tools, and
   work on a common goal. Keep game connections alive independently of model runs.
4. **Human participation:** the owner joins the agents' world, directs a castle
   build, receives a stone-brick-versus-cobblestone question through Discord and
   steers the same task without restarting the team.

Implement the shared prerequisites incrementally: declared build environments
and long-running commands → interactive browser and structured observations →
bounded parallel/persistent workers and shared tools → game adapter and bridge
handoff. Storage, recovery and hardening checks below gate the corresponding
work. A small website does not need to wait for autonomous bots.

Non-Rust third-party tooling is allowed only inside tool/agent containers.
Control and authored integrations remain Rust/Haskell/Lean/WASM/Nix; Mineflayer
can be an isolated upstream dependency behind a Rust adapter.

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

## Next: core, web and terminal boundaries

Prioritize this bounded reorganization before expanding persistent services.
The existing server still combines mutable application state, routing, rendering
and execution assembly; `control-tui`, `control-web` and `control-core` are not
separate crates yet. Keep `main` buildable throughout small reviewed slices.

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

Do not expose the full state mutex as the interface, clone complete histories,
introduce a distributed framework or make every tool its own crate. This improves
the seam for later remote execution; authentication, resource ownership and stale
execution fences still need their own implementation and tests.

## Next: persistent worker/tool environments

Add persistent worker/tool environments for long-running tools and services.
Agents attach to an explicit environment identity; model attempts do not own its
entire lifetime. Reuse command-job and preview supervision, with immutable Nix
bases, private writable state, resource leases and explicit stop/restart/expiry
policies. Keep provider credentials out of tool containers. Existing checkpoint
barriers still quiesce source writers; persistent services need declared read/write
scope and recovery semantics rather than silently bypassing those barriers.

Keep bounded slots, per-chat FIFO ordering, isolated workspaces and captured
provider/model selection. Preserve exact resource ownership, cancellation cascades,
stale-result fences and restart reconciliation as service lifetimes expand.
Add aggregate node resource budgets before increasing fan-out further;
deterministic Rust supervision remains responsible for scheduling.

## Next: reliability and real-user feedback

Finish these before increasing fan-out further, unattended operation or spending.

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
- Add admitted knowledge search/read and bounded claim proposals, authorized room
  posting in the external bridge, and `control.run.submit/status/diff` using the
  same durable run service. External callers inherit no worker/operator authority.
- Expose useful bounded preview diagnostics; keep service identity and ingress
  transport separate. Add scoped, revocable remote MCP or preview transport over
  Tailscale/SSH when a real user needs it. Private networking is not authentication.
- Provide declarative Nix environment/tool requests. Use pinned inputs and private
  writable stores/profiles/build state, with bounded resources. Agents can use
  advertised environment tools freely within their grant; package fetching is an
  explicit network capability. Never expose the host daemon or engine socket.
- Keep shared images immutable. Cache reads and publication are separate policies;
  never promote a worker's filesystem or generated output into a shared base/cache
  automatically. Defer caching complexity until measured reuse justifies it.
- Add API/self-hosted adapters through the same bounded executor contracts. Personal
  subscription use and node authority remain separate; operators must comply with
  provider terms. Authorized API-backed paths with budgets are the unattended path.

## Next: visible multi-model review turns

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
node reliability and the Minecraft/bot demonstrations ahead of distributed
frameworks or broad rewrites.

## Later: personas as persistent bots

A bot is a supervised instance using a persona, tools, state and explicit grants,
not a second agent runtime. Preserve the original personality + policy +
declarative-directive model; keep its configuration distinct from its chat history.

- **Persona definitions:** declarative personality, behavior directives, lens
  composition, model selection and limits. Validate structure and enforceable
  policy before launch. Clearly separate mechanically enforced constraints from
  natural-language preferences; do not claim to verify arbitrary model behavior.
- **Durable callbacks:** a small declarative workflow/event interface with typed
  inputs, timers/subscriptions, bounded retries, deadlines and explicit transitions.
  The Rust supervisor handles scheduling, deduplication, leases/fencing, restart
  and cancellation; a model decides only where judgment is needed. No generic
  programming framework until concrete workflows require it.
- **Bot spawning:** give bots a tool to request children with bounded depth,
  concurrency, token/spend/storage budgets and inherited authority ceilings.
  A child cannot grant itself or its parent more access. Existing human `/spawn`
  and bounded worker-to-worker delegation are implemented, with bounded parallel
  model runs. Persistent autonomous bots and independent service lifetimes remain
  planned.
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

## Deferred

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
