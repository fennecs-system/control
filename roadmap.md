# Proposed roadmap — for review

2026-10-29. Ship a useful macOS-with-Nix build to a few friends and learn from
their actual work. Each runs their own node with their own model access.

The invited release must preserve work between turns, fail safely, recover after
restart, and install without editing source. Selecting Claude inside Control is
a high-priority parallel track. Full provider parity and federation come later.

**Augment, don't intermediate:** people bring their own model access. Control
adds environments, tools, knowledge and review around it. The same capabilities
serve external MCP clients and Control-managed runs; the web, TUI and Discord
are interfaces to those capabilities.

Implementation progress (2026-10-29): HTTP socket writes now release shared state,
stylesheets bypass it, Discord ingestion has its own thread, and individual
progress events no longer fsync the full state. Discord principals have moved to
private configuration with arbitrary labels/roles. Node task access is independent
of model provider and architect authority; multiple Discord IDs may be one person.
Configuration and state now resolve under independent home-directory roots. The
binary embeds UI assets, the security profile, TUI and Discord bridge; offline
doctor checks and a single-writer state lock are implemented. Ordinary chats
restore bounded published snapshots, and the artifact explorer/viewer shares
list/read services with MCP. Cancellation and restart reconciliation now validate
known resource ownership and confirm exit; unknown or mismatched resources block
dispatch. Completion fencing preserves the last published checkpoint. Explicit
`/steer` follow-ups wait for the next turn; `/btw` and `/spawn` create linked,
independent task workspaces from published checkpoints, with one execution slot
and at most 32 pending runs. Hard live-workspace quotas, persisted daemon IDs at
launch, graceful signal shutdown, Claude refresh/fault verification and broader
release gates remain open. Friends
build the ordinary release binary from main with Nix; there is no separate preview
target. A working local build does not close those reliability and provider gates.

Design-decision interaction: managed Codex/Claude runs can ask a bounded structured
question with options and a recommendation. Execution yields after verified stop
and an immutable checkpoint; pending questions survive restart. A human answer
queues an actor-bound continuation with the captured model/context. Only that
chat's follow-ups wait; recommendations grant neither consent nor capabilities.
Web, TUI and Discord share the same answer service. Routine sandbox operations
remain autonomous; asking at the right design boundary is prompt-guided.

## 1. Make a second Mac configurable — release blocker

- Keep validated node files under `~/.config/control/`, with an explicit
  directory override and documented precedence. Keep mutable
  state outside the checkout; resolve paths consistently regardless of cwd.
- Configure state/artifact roots, pinned images, source root for explicit
  improvements, provider/model selection, credential references, and limits.
  Locate packaged WASM assets and seccomp policy without author-specific paths.
- Store only opaque credential references: a private file path or named
  environment input consumed by its supported trusted adapter. No literal
  credentials in config examples, source, state, logs, prompts, images or the Nix
  store. Do not inherit the host environment into workers. Preserve Codex's
  metadata-only host check and read-only credential mount; do not convert it to
  an environment-secret path. Discord secrets belong only to the activated bridge.
- Replace source-fixed Discord labels/roles with explicit numeric-ID bindings
  and display labels in private configuration. No real IDs as defaults. Preserve
  DM-only routing, confirmed recipient/channel mappings, denial of unknown actors,
  user/architect restrictions, and checks at ingress and dispatch. A role does
  not establish ownership of a provider subscription.
- Authorize model-backed tasks through node policy, independent of provider and
  architect authority. Multiple Discord IDs can belong to one person and retain
  the user role; DMs address that node. Recheck at enqueue, retry, dispatch and
  tool calls. Do not infer distinct people or subscription ownership from Discord
  IDs or roles. Include the provider-terms notice: making a personal account
  available to others may breach [OpenAI's terms](https://openai.com/policies/terms-of-use/)
  and risk suspension; operators are responsible for compliant configuration.
- Add offline `control doctor` checks for configuration, private-file metadata,
  state permissions, engine, pinned image availability, assets and sandbox policy.
  Keep authenticated model checks explicit and separate from offline diagnostics.

**Exit:** a clean second macOS account starts from documented commands and a
placeholder-only config; starting from another directory opens the same state.
Missing prerequisites explain a fix and never launch a host worker. Discord
policy tests pass with arbitrary fixture IDs and existing DM isolation intact.

## 2. Preserve work and make interruption safe — release blocker

Depend on phase 1. Keep a small Rust core: typed transitions, immutable attempt
inputs, one deterministic supervisor and concrete adapters; no framework migration.

- Capture durable intent before dispatch: actor, action, conversation revision,
  resolved context/tools, provider/model, image digest, source/baseline identity,
  limits and credential-owner reference. Retrying records a new attempt without
  silently changing these inputs; changed inputs require an explicit new revision.
- Give each conversation a durable workspace lineage, separate from its
  transcript and provider continuation token. Seed a fresh attempt from the
  last published workspace checkpoint; publish by an atomic version change only
  after a matching attempt finishes and artifacts pass validation. Serialize
  writers per conversation: queued follow-ups wait in submission order and capture
  the preceding result/workspace when dispatched; steering must be explicit.
  Preserve failed/interrupted artifacts for inspection;
  never silently promote partial work or overwrite the previous good checkpoint.
- Keep ordinary workspaces separate from authorized Control source snapshots.
  Exclude harness mailbox/results/configuration from copy-forward; reject unsafe
  paths, symlinks and oversized exports. Retain durable conversation history and
  show when the model's bounded context omits older turns. Filesystem continuity
  does not depend on resuming a vendor CLI session.
- Enforce a hard writable-storage bound on the supported macOS engine, including
  scratch, artifacts and export staging. Prototype one quota-backed persistent
  store or bounded scratch plus durable checkpoint scheme; verify actual engine
  enforcement and crash behavior. Plain bind mounts, directory polling and a
  Docker volume name alone do not enforce a quota. Bound retained node storage
  and stop admission when its budget is exhausted; cleanup preserves references.
- Persist stable node/attempt identity and exact daemon container IDs around
  launch. Acquire a single-writer state-directory lock. On startup reconcile
  durable intent against inspected node/run/attempt ownership, mounts and image;
  stop or adopt only verified owned containers. Handle a crash between container
  creation and recording its ID. Unknown or mismatched resources block affected
  work for inspection; never delete every labeled container or unrelated jobs.
- Fence completions and mailbox requests by active attempt/generation. Persist
  external-effect intent and deduplication keys before acknowledgements. Reuse
  room/Discord deduplication where available; ambiguous remote effects require
  inspection rather than automatic replay. Container termination cannot undo a
  posted message. Do not promise exactly-once execution.
- Add cancel and graceful shutdown to the shared run service; stop the exact
  owned container and confirm its exit before releasing its workspace or retrying.
  Preserve interrupted status when outcome or cleanup remains uncertain.
- Keep DM ingestion and request handling responsive while a worker runs. Start
  with one execution slot and a visible bounded queue; broader worker concurrency
  can follow safe workspace serialization and recovery. Enforce broker tools at
  listing and invocation against captured lens selection, node policy and actor.
  Public web search is explicit egress; record bounded, privacy-conscious evidence
  without persisting secrets or raw private queries as a new audit channel.
- Keep network writes outside the shared state lock, bound connection/read/write
  waits, and serve static assets without waiting for a run or checkpoint. Verify
  chat submission, navigation and Discord ingestion during a long-running worker
  and a slow client; a healthy execution thread alone is not a responsive UX.
- Remove panic assumptions at mutable request/worker boundaries. Validate and
  reload a known-good checkpoint, or fail closed with a restart diagnostic, after
  poisoned state; blindly accepting a poisoned mutex is not recovery. Batch
  disposable progress updates separately from durable lifecycle/effect commits.
  Preserve write ordering, atomic replacement and directory durability; disk-full
  errors must not acknowledge uncommitted state or lose the previous checkpoint.

**Exit:** scripted tests create a project, add tests in a second turn, restart,
and continue with the same files/context provenance. Inject termination before
and after launch/checkpoint/result publication, cancellation, stale completions,
duplicate tool requests and disk-full errors. No competing workspace writer,
duplicate acknowledged room effect, false success or unrelated-container cleanup
occurs. A real supported-engine quota test fails writes at the configured bound;
the Codex credential/PID canary still gates real credential mounting.

## 3. Invite friends, with a useful Claude entry point

Release after phases 1–2 pass; prepare documentation and Claude connection alongside.

- Rewrite README onboarding for another Mac: supported host/engine prerequisites,
  Nix setup, repeatable build/install commands, pinned worker image acquisition,
  config template, own-account login, doctor, first two-turn task, restart/cancel,
  artifact review and optional Discord setup. Remove author paths, real IDs and
  credentials from onboarding/service examples. Provide a reproducible supported
  image path; publishing a cache is useful if local Linux-image building dominates
  setup time, but a registry architecture is not itself a release gate.
- Verify the existing `control mcp` stdio bridge with the user's own Claude Code
  as an optional complementary workflow. Document its read-only Hive chat,
  lens/room tools, `control.web.search`, `control.rust.run`, offline browser capture,
  prerequisites and failures. It supplies no generic worker file/shell/source
  editing and does not let users select Claude inside Control. A plugin package
  is optional. Claude documents local MCP setup
  in its [MCP guide](https://code.claude.com/docs/en/mcp).
- Extend that quickstart with knowledge search/read and explicitly authorized
  room posting; these are additions, not tools already present in the stdio
  bridge. Use the shared registry below and remove checkout-dependent startup.
- If friends need another machine's node, promote scoped remote MCP over
  Tailscale into this cohort's work. Require per-principal revocable access and
  tool authorization; private networking alone is not authentication. Do not
  wait for the construction loop or federation, and do not expose the universal
  operator session as a remote-client credential.
- Keep a short known-limitations list and a manual feedback template: task,
  expected/actual result, build/platform, optional redacted run ID and artifact.
  Let the friend inspect anything exported. Start with direct feedback; a new
  feedback room, command and telemetry system are not prerequisites.

**Exit:** two friends independently install from README, complete a two-turn
construction task and restart it; one verifies the Claude MCP workflow. Record
setup blockers, lost-work incidents and useful/missing behaviors, then triage
feedback before adding scope. No week-long soak prerequisite. A Codex-only pilot
may start sooner. **Release decision for review:** require the in-Control Claude
adapter gate below for the first Claude-user cohort, or explicitly invite that
cohort to an external-MCP-only preview. Do not count MCP as provider completion.

## 4. Parallel priority: Claude inside Control

Implementation update: backend/model selection is now captured per run, with
backward-compatible Codex defaults. The pinned Claude Code container adapter,
explicit login, bounded stream parsing and cache accounting are implemented.
Built-in Claude tools are disabled; workspace commands use separate offline,
credential-free containers and durable command admission. Its offline isolation
and workspace-continuity tests pass. Authenticated Claude two-turn work, restart
continuation and artifact viewing now pass on this Mac. Refresh and cancellation
fault tests remain open; do not count this as phase completion.
The external `control mcp` direction remains available independently.

Develop this adapter alongside configuration and recovery, reusing phase 2's
supervisor contracts. Its runtime release depends on those safety gates. Choose
one minimal personal Claude path; prioritize it over federation and broad tooling.
After extracting the Codex backend seam, Claude CLI is the first new adapter.
Keep the seam small; broad provider parity is not a prerequisite.

- First timebox an unmodified, pinned Claude Code CLI adapter for a credential
  owner's interactive work on their own node: capture selection per attempt,
  translate text/final/usage/error events, and reuse supervisor limits/cancel.
  Authenticate only through Anthropic's supported flow. Never extract OAuth
  tokens, impersonate Claude Code, or route other people's work through the
  operator's subscription. Recheck node actor authorization at enqueue and dispatch;
  several user-role identities may represent the credential owner.
- This path ships only after a Claude-specific credential/PID isolation probe,
  safe config/hook/MCP discovery and credential refresh behavior are demonstrated
  on the supported image/engine. Codex's canary proves nothing about Claude.
  `--bare` does not use subscription login, per the official
  [CLI guide](https://code.claude.com/docs/en/headless); do not combine those claims.
- If that safety gate cannot be met promptly, keep Claude CLI external via MCP
  and implement one native Anthropic API-key adapter instead: a trusted host
  adapter resolves a private file/environment reference, while credential-free,
  offline tool containers execute authorized tools. Add only the minimal bounded
  tool loop, typed transcript/tool results, streaming parser, usage normalization,
  cancellation and explicit spend limits. No Bedrock/Vertex/router requirement.
  The native API adapter is also the intended backend for authorized shared or
  unattended work; it is not a subscription credential workaround.
- Official [hosting/auth conditions](https://code.claude.com/docs/en/legal-and-compliance),
  checked 2026-10-29, permit users signing into the unmodified CLI themselves,
  subject to the stated product-hosting conditions; they prohibit subscription
  credential intermediation. Native API keys have a different billing/auth boundary.
  Recheck before shipping; third-party bridges do not establish permission.

**Exit:** the chosen Claude path completes the same two-turn/restart task and
passes malformed-stream, auth-expiry, cancellation and unauthorized-actor tests. UI
labels distinguish subscription/API usage and unknown accounting. No secret
reaches model-readable tools. Add bounded parallel workers, richer diffs and
independent verification next when feedback justifies them; storage/event-log
migration, streaming polish and session rotation remain separate improvements.

## 5. Shared MCP capabilities and the construction loop

Build useful capabilities once, with transport-specific grants rather than three
hand-maintained catalogues. This work can ship incrementally alongside the pilot.

- Chat artifacts, first version: a compact file explorer and read-only text
  viewer for completed run outputs, linked from the chat. Use an immutable,
  bounded manifest with chat/run/attempt provenance, relative paths, sizes,
  content types and hashes. Reject traversal, symlinks and harness/private files;
  escape text and never execute HTML or scripts. Expose the same bounded
  list/read service through MCP with actor checks. Files do not automatically
  become knowledge. Basic editing and syntax highlighting follow the viewer;
  the manifest can represent other output types without implementing them now.

- Share tool definitions, schemas and policy dispatch across HTTP MCP, stdio and
  the worker mailbox. Keep authority explicit: an external client does not
  inherit a worker's active-run capabilities or the operator's subscription.
- Add knowledge search/read, bounded evidence-backed claim proposals and
  authorized room posting. Lens tools expose explicit snapshots. Proposals need
  admission and review; chat output must not automatically become knowledge.
- Add `control.run.submit`, `control.run.status` and `control.run.diff` on the
  same durable run service. Submission obeys provider configuration, actor policy,
  queue/storage bounds and attempt fencing. A diff is reviewable evidence, not
  permission to apply a change.
- Audit `readOnlyHint` and `openWorldHint` against actual effects and test the
  policy boundary. MCP annotations describe behavior; authorization enforces it.
- Follow with independent test evidence, explicit acceptance and provenance.
  Cross-provider judging records author and reviewer identities and limits.
  Default to one owner-initiated review per run, subject to current provider
  terms and limits; that cap is not proof of subscription permission. Use
  authorized API-backed paths with explicit budgets for judge fan-out, evals,
  unattended agents and swarms.

**Exit:** an external MCP client can read admitted knowledge, post only to an
authorized room, submit an allowed job and inspect its status/diff. Transport
parity and denial tests prevent catalogue drift and privilege escalation.

## Deferred scope and corrections to the review

Other platforms, NixOS packaging, live federation/delegation,
general tool installation, swarms, model routing and bundled inference come later.
Keep the existing offline federation boundary and small pure Rust context core.
Remote MCP is conditional pilot scope as above, not a federation prerequisite.

The review correctly identifies fresh workspaces, serial ingestion, whole-state
activity saves, source-fixed principals and missing crash reconciliation in current
source. Its fixes understate their coupling: poison recovery needs invariant
validation, labels alone cannot establish cleanup ownership, and tmpfs alone is
not durable storage. Its credential-free vision is future work: today's trusted
Codex harness does read a mounted credential, outside its sandboxed agent tools.
The stdio bridge is narrower than the proposed Claude plugin catalogue. The UI
already includes Rust/WASM behavior; no UI rewrite or Enter-to-send work belongs
on this roadmap. Detailed Claude model names, limits and beta flags in the review
are not requirements here; verify current primary docs when implementing.

Proposed order: node authorization independent of architect roles and responsive ingestion/UI →
portable private configuration and the backend seam → Claude CLI as the first
new adapter alongside durable bounded work and safe recovery → invited macOS
cohorts with explicit provider gates and useful MCP capabilities → improvements
selected from actual feedback. Prepare onboarding throughout; bring scoped
remote MCP forward when that is what lets a friend participate.
