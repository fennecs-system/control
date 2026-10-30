# Control architecture

Design direction, 2026-10-29. This describes the intended system; the current
implementation boundary is recorded below.

## An augmentable construction tool

Control is a general-purpose constructor for code and the systems built from it.
It brings together a harness, an MCP server, knowledge and conversations,
declarative execution environments, and potentially a model-calling service and
router. These are parts of one tool that a person can run, understand, extend,
and improve on their own machine or infrastructure.

The ambition extends beyond a coding chat client: combine intent, explicit
context, tools, agents, and verification to produce reviewable changes and running
systems. Keep ordinary deterministic work in code rather than spending model
tokens on scheduling, permission checks, retries, or cleanup.

Rust, functional programming, and Nix are foundations. Use typed data and explicit
state transitions, keep the context core pure, and isolate effects behind small
contracts. Nix declares reproducible dependencies and deployments; Rust supplies
the native server, harness, and tools; WASM provides browser components. Control's
application and integration layers have no Node runtime or authored JavaScript/Python.
These choices rule out classes of mistakes; they do not prove model behavior or
distributed effects correct.

Non-Rust third-party tooling may run inside tool or agent containers. Control
and authored integrations remain Rust/Haskell/Lean/WASM/Nix; an upstream runtime
inside an isolated tool is not a host integration runtime. The
[demonstration goals](DEMONSTRATIONS.md) exercise website construction, browser
research with a tool view, and coordinated Minecraft bots using these boundaries.

## One node, distinct responsibilities

A future persistent bot binds a versioned persona, composable lenses, policy,
resource grants, limits and conversations. Its identity outlives any run or
container. Declarative event/callback definitions reuse the deterministic
supervisor; child bots inherit authority ceilings and budgets. Human questions
reuse the bridge, while capability grants and consequential external actions
remain separate authorization decisions. See the [roadmap](../roadmap.md) and
[inspiration review](BOT-INSPIRATIONS.md); this is direction, not implemented
unattended automation.

Project grants, isolated worktrees and local preview services are described in
[Projects and previews](WORKSPACES.md). Resource intent and authorization belong
to the node; agents invoke tools against those captured resources. Base images
stay immutable. Environment changes belong in private worker state or separate
tool containers. Nix package acquisition and shared cache publication are
distinct capabilities, not implicit consequences of being inside a sandbox.

A node defines its authorized identities and roles independently of its model
backend. Several Discord IDs can represent one person, all with the user role.
Task/chat permission never grants Control source editing, settings or an operator
session; source improvements require architect authority. A DM addresses the
node attached to that bot, not an implicit global model account. Subscription,
API and self-hosted adapters share this authorization boundary. Operators remain
responsible for provider terms; permissions cannot certify contractual compliance.
Personal node configuration lives under `~/.config/control/` (or an explicit
configuration directory), outside the checkout and worker source snapshots.

| Boundary | Responsibility |
| --- | --- |
| Harness and durable state | Conversations, run/attempt identities, context provenance, task progress, recovery rules, cancellation, and usage. A conversation outlives a process or container. |
| MCP and tool broker | Describe tools and authorize each invocation against the original actor, local policy, and captured constraints. UI and agent callers use the same contracts. |
| Nix execution | Declare tool closures, worker images or microVMs, resource limits, and deployment services. A reproducible environment and an enforced sandbox are separate requirements. |
| Model adapters and optional router | Translate model requests, events, tools, and accounting. A future router can choose an authorized backend under declared cost, capability, and data-location constraints. |
| Clients | Rust terminal, web/WASM, Discord, and other adapters attach to the same node-owned conversation state. They do not each implement an agent runtime. |
| Federation | Exchange explicitly shared resources and, later, bounded work between independently administered nodes. Peer identity and local operator authority remain separate. |

Lenses remain pure, composable functions over explicit snapshots. They select
context and tool descriptions without ownership or effects. The execution layer
enforces which selected tools may actually run. Personas guide behavior; policy
and deterministic supervision enforce authority.

## A deployable collective tool

Imagine distributing Control together with a Nix configuration and an optional
model: a declared environment that can become a useful node without relying on
one operator's laptop or a mandatory central service. Hosted model APIs remain an
option; compatible local inference could make a deployment independent of them.

The target bundle identifies the Control revision, Nix inputs and closures,
worker/tool image digests, enabled services and capabilities, resource requirements,
and optional model weights and inference runtime. Distribute the native Control
executable with embedded UI assets, plus the declared dependencies it needs.
Mutable conversations, workspaces, node identity, and credential material live
outside that immutable bundle. Replacing a deployment must preserve its state;
rolling back code does not roll back external effects.

Provide a NixOS module for a complete declarative Linux node, alongside packaging
for supported Nix hosts. Portability has an explicit platform contract: Linux
containers and microVMs need a compatible runtime, and local models need suitable
memory, hardware, drivers, and redistribution terms. Nix alone does not supply
those guarantees.

Anyone should be able to adapt their own node and contribute improvements. On a
shared node, existing operator roles, review, and deployment policy still apply.
Self-improvement produces attributable source/configuration changes and evidence
for the receiving operator to accept.

## Federation as interoperability

Nodes should be able to find and connect with other nodes, share useful lenses,
tools, knowledge, and collaboration spaces, and eventually delegate constrained
work. Social-style connections make a collective useful without requiring a
single shared administrator or database. Participation is optional; one node must
remain useful on its own.

Share versioned descriptions and explicit artifacts, not ambient trust. Importing
a peer resource cannot install code, evaluate Nix, activate capabilities, or grant
source-editing authority. Treat executable extensions differently from inert
context. Prefer small versioned protocols and capability negotiation over assuming
every peer runs the same build. See [the federation specification](FEDERATION.md).

## Lessons to evaluate from Pi Durable

[The Pi Durable post](https://earendil.com/posts/pi-durable/) offers useful semantics:
checkpoint model/tool steps, persist intent before effects, deduplicate submissions,
classify replay safety, give child work explicit cancellation/lifetime relationships,
and keep compaction observable without deleting history. It also separates storage
from execution environments. Borrow these ideas as requirements to evaluate, without
adopting its implementation. Local request deduplication does not guarantee
exactly-once external effects.

Control should add immutable execution provenance and explicitly authorized
federation to that discussion. Prefer a small typed core over a general mutable
hook framework. Judge the result through restart/replay tests, reproducible
deployments, bounded resources, and usable interfaces; language choice alone does
not establish that Control exceeds another harness.

This review makes no runtime changes. The next architectural dependency remains
an immutable execution manifest and explicit recovery semantics, as recorded in
[the earlier investigations](RESEARCH-NOTES.md).

## Current implementation boundary

Control currently has a Rust local server and thin TUI, persisted conversations
and attempts, composable lens snapshots, Codex and Claude Code Nix containers,
selected brokered tool integrations, and signed offline federation resource exchange. A restart marks
in-flight runs interrupted; it does not resume individual model/tool steps.

The macOS release binary embeds the UI, security profile, TUI and Discord bridge.
Configuration and state have independent home-directory roots. Published file
snapshots give ordinary chats workspace continuity; a state pointer advances only
with a committed successful result. Artifact list/read is one bounded service used
by the chat explorer/viewer and MCP. Output hashes, relative paths and attempt
provenance describe files without making them accepted knowledge or authority.
The first viewer executes no content and performs no edits.

Improvement is a persistent architect-authorized chat mode rather than a separate
prompt page. Conversation history follows the discussion and implementation;
ordinary-task and source-improvement workspaces keep independent checkpoints.
Each queued turn captures its action, and each message/dispatch/tool call checks
the original actor. Discussion behavior is instructed through the prompt;
filesystem isolation and the lack of live application/deployment are enforced
separately. Agents shows current execution and the queue, while Runs keeps history.

Long commands are durable broker jobs in credential-free offline containers,
separate from provider execution. An operator-installed Nix build image supplies
immutable vendored dependencies; output and scratch are private to the task.
Delegation checkpoints the parent, releases the serial execution slot, runs a
bounded child and resumes with scoped retained feedback. Different-provider judges
have read-only source and cannot publish or advance a workspace. Deterministic
admission, scheduling and publication fences remain Rust supervisor duties.
See [command jobs](COMMAND-JOBS.md), [build environments](BUILD-ENVIRONMENTS.md)
and [delegation](DELEGATION.md).

The optional [GitHub broker](GITHUB.md) binds publication to private node
configuration, checks captured source against the remote base and opens draft PRs
from completed improvements. Provider credentials and GitHub credentials have
separate consumers; workers never receive the GitHub token. Publishing does not
merge or deploy. The [threat model](THREAT-MODEL.md) records enforced boundaries
and remaining gaps.

A persistent execution environment is a broader resource than a persistent shell:
it can retain project files, shells, build state and supervised services while
agents attach, stop or change provider. Keep environment identity and lifetime
separate from chat history and individual model attempts. Its declarative Nix
base stays immutable; writable state, network grants, leases, resource limits and
cleanup belong to the Rust supervisor. A container or microVM is an isolation
choice, not a reason to grant the model host administration.

Today Control retains file checkpoints and some preview services, and recreates
provider containers per attempt. General persistent worker containers and Nix
VMs remain future work. Restoring files does not restore live process state;
restarting a lost service must follow explicit recovery semantics. Persistent
shells may be useful tools within those environments without defining the agent
runtime or weakening reproducibility.

Backend/model selection is captured per run. The Claude adapter runs an unmodified,
pinned CLI with built-in tools disabled and an explicit Control MCP bridge.
Its harness sees a mailbox, while a separate offline container mounts the task
workspace without any login or harness process. Shared node authorization gates
tool calls; durable admission prevents replaying an ambiguously acknowledged
workspace command. The external `control mcp` entry point also remains available
to a user's independently run Claude Code. See [both paths](CLAUDE.md).

A state-directory lock prevents competing servers. Durable interrupted attempts
and node labels prevent dispatch alongside earlier active containers; unknown
containers require inspection, not guessed cleanup. Live workspace disk quotas
and automatic container reconciliation remain open before broader unattended use.

A complete packaged node, NixOS module, native multi-provider loop/router, bundled
local inference, durable task recovery, safe child-run scheduling, and networked
federation remain future work. The existing browser transcript refresh and TUI
revision polling are not a durable incremental event subscription API.
