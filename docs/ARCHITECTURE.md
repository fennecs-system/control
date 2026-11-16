# Control architecture

## Purpose

Control is a constructor for code and the systems built from it. It combines a
harness, an MCP server, knowledge and conversations, declarative execution
environments and model adapters into one tool that a person can run, understand
and extend on their own machine.

Deterministic work stays in code: scheduling, permission checks, retries and
cleanup do not spend model tokens. Rust supplies the server, harness and tools;
WASM provides browser components; Nix declares dependencies, images and
deployments. Control's own code and integrations are Rust/Haskell/Lean/WASM/Nix
with no Node runtime or authored JavaScript/Python. Third-party tooling in other
languages runs only inside tool or agent containers, and task-specific servers
and adapters live in those environments, not in Control's source. These choices
rule out classes of mistakes; they do not prove model behavior or external
effects correct.

## Responsibilities

| Boundary | Responsibility |
| --- | --- |
| Harness and durable state | Conversations, run/attempt identities, context provenance, recovery, cancellation and usage. A conversation outlives any process or container. |
| MCP and tool broker | Describe tools and authorize each call against the original actor, node policy and captured constraints. UI and agent callers use the same contracts. |
| Nix execution | Declare tool closures, worker images, resource limits and deployment services. A reproducible environment and an enforced sandbox are separate requirements. |
| Model adapters | Translate requests, events, tools and accounting for Codex, Claude Code and local endpoints. Provider choice does not change tools, permissions or publication. |
| Clients | The web UI, terminal and Discord attach to the same node-owned state; none implements its own agent runtime. |
| Federation | Exchange explicitly shared resources between independently administered nodes. Peer identity and local operator authority stay separate. |

Identity and roles belong to the node, independently of the model backend.
Several Discord IDs can represent one person. Chat permission never grants
source editing, settings or an operator session; source improvements need the
architect role. Operators remain responsible for provider terms. Personal
configuration lives under `~/.config/control/`, outside the checkout.

Lenses are pure, composable functions over explicit snapshots. They select
context and tool descriptions; the execution layer decides which selected tools
may run. Personas guide behavior; policy and deterministic supervision enforce
authority.

Domain transitions take narrow typed state and return effect descriptions; the
runtime interprets those effects (checkpointing, containers, networking, model
calls). Clients submit commands and read projections rather than sharing the
whole application state. See [state and effect boundaries](STATE-BOUNDARIES.md)
and [library boundaries](CRATE-BOUNDARIES.md).

## How it works now

Control is a Rust local server with a web UI, a thin terminal client and a
Discord bridge. The release binary embeds the UI assets and the worker seccomp
profile. A state-directory lock (`state.lock`) prevents competing servers.
Configuration and state have independent roots.

- **Runs and workspaces.** Backend and model are captured per run. Ordinary
  chats keep published file checkpoints; a chat's pointer advances only with a
  committed successful result. Restart marks in-flight runs interrupted and does
  not replay model or tool steps; unpublished work is kept as a recovery
  candidate. Project grants, isolated worktrees and previews are described in
  [projects and previews](WORKSPACES.md).
- **Providers.** Codex and Claude Code run in pinned Nix containers. Claude's
  built-in tools are disabled; its harness sees a mailbox while workspace
  commands run in a separate offline container without credentials (see
  [Claude](CLAUDE.md)). Local models run through Control's native tool loop
  (see [local models](LOCAL-MODELS.md)).
- **Commands and builds.** Long commands are durable broker jobs in
  credential-free offline containers. An operator-installed Nix build image
  supplies vendored dependencies. See [command jobs](COMMAND-JOBS.md) and
  [build environments](BUILD-ENVIRONMENTS.md).
- **Delegation.** A parent checkpoints, releases its execution slot and resumes
  with the child's result. Judges get read-only source and cannot publish or
  advance a workspace. Spawn batches have bounded admission and a durable
  backlog. See [delegation](DELEGATION.md).
- **Persistent services.** Services, their data and model attempts have
  independent lifetimes. Services keep a retained `/workspace` volume and an
  explicit restart policy; provider containers are recreated per attempt.
  Restoring files does not restore live process state. Scoped
  [TCP grants](PORTS.md) connect verified services through trusted relays while
  their container network stays disabled. See
  [persistent tools](PERSISTENT-TOOLS.md).
- **Artifacts.** One bounded service lists and reads published files for the
  web viewer and MCP. Hashes, paths and attempt provenance describe files
  without making them accepted knowledge or authority. The viewer executes no
  content.
- **Self-improvement.** Improvement is an architect-authorized chat mode with
  its own checkpoints. The optional [GitHub broker](GITHUB.md) opens draft PRs
  from completed improvements; workers never receive the GitHub token, and
  publishing does not merge or deploy.
- **Bots.** Operator-approved bots are chats that sleep until room messages,
  service events or timers wake them. The pure `control_bot::Supervisor`
  decides; `src/bots.rs` queues ordinary turns within budgets. See
  [watches](WATCHES.md) and [room notifications](ROOM-NOTIFICATIONS.md).
- **External harnesses.** `control mcp` gives an independently run Claude Code
  or Codex CLI read access and approval-gated requests; it cannot submit jobs or
  administer Control (see [Claude](CLAUDE.md)).

The [threat model](THREAT-MODEL.md) records the enforced boundaries and known
gaps. Live workspace disk quotas and some ambiguous container-creation recovery
cases still limit unattended use. Unknown containers require inspection, never
guessed cleanup.

## Direction

- **Deployable bundles.** A bundle would identify the Control revision, Nix
  inputs, image digests, enabled capabilities, resource needs and optional model
  weights. Mutable conversations, workspaces, identity and credentials stay
  outside it; rolling back code does not roll back external effects. Linux
  containers and local models still need a compatible runtime, hardware and
  redistribution terms. See [NixOS](NIXOS.md) for the current packaged path.
- **Federation.** Nodes share versioned lenses, tools and knowledge, and later
  delegate constrained work. Importing a peer resource never installs code,
  evaluates Nix, activates capabilities or grants authority. One node must stay
  useful on its own. See [federation](FEDERATION.md).
- **Durable execution.** [Pi Durable](https://earendil.com/posts/pi-durable/)
  is a reference for checkpointed steps, intent before effects, replay-safety
  classification and observable compaction. Local deduplication does not make
  external effects exactly-once.
- **Bots and teams.** Personas, schedules and reviewed self-improvement build
  on the existing bot supervisor rather than new core concepts. See
  [inspirations](BOT-INSPIRATIONS.md) and the [roadmap](SWARM-ROADMAP.md).

Nix VMs, effect-safe resumption of individual steps and networked federation
remain future work.
