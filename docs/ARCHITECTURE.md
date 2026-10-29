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
the native server, harness, and tools; WASM provides browser components. No Node
runtime or authored JavaScript/Python application layer is part of this direction.
These choices rule out classes of mistakes; they do not prove model behavior or
distributed effects correct.

## One node, distinct responsibilities

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
and attempts, composable lens snapshots, Codex-backed Nix worker containers,
selected brokered tool integrations, and signed offline federation resource exchange. A restart marks
in-flight runs interrupted; it does not resume individual model/tool steps.

A complete packaged node, NixOS module, native multi-provider loop/router, bundled
local inference, durable task recovery, safe child-run scheduling, and networked
federation remain future work. The existing browser transcript refresh and TUI
revision polling are not a durable incremental event subscription API.
