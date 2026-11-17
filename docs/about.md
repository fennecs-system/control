# About Control

Control adds environments, tools, knowledge and review around your own model
access. It is a Rust harness and server with an MCP surface, a Rust/WASM web UI,
a terminal UI, an optional Discord bridge and Nix-built execution environments.
Each node keeps its own data, configuration and authority.

This page is a map. [ARCHITECTURE.md](ARCHITECTURE.md) describes how the parts
fit; [DESIGN.md](../DESIGN.md) keeps the original prompts; the
[roadmap](SWARM-ROADMAP.md) lists open work.

## Chats, runs and workspaces

The web UI, terminal and Discord share persistent chats and one durable run
service. Each chat has one writer; the node runs a bounded number of model
attempts at once and queues the rest ([execution limits](EXECUTION.md)). A
project restores its last published workspace for each turn, so continuity does
not depend on resuming a provider session ([turn continuity](TURN-CONTINUITY.md),
[workspaces](WORKSPACES.md)). `/spawn`, `/btw` and delegation create linked child
chats with their own workspace copies ([delegation](DELEGATION.md)).

Cancellation stays **Stopping** until container exit is confirmed. At startup,
Control checks persisted ownership and stops only verified containers; unknown
resources pause dispatch for inspection. Failed, cancelled and interrupted work
never replaces the last good checkpoint.

Only explicit, authorized `/improve` runs receive a snapshot of Control's own
source. Changes never apply to the live source or installation automatically
([GitHub tool](GITHUB.md)).

## Models

Codex is the default backend; Claude Code ([CLAUDE.md](CLAUDE.md)) and local
OpenAI-compatible endpoints ([LOCAL-MODELS.md](LOCAL-MODELS.md)) are also
supported. Usage records provider-reported tokens per attempt; missing telemetry
stays unknown and is not a bill.

Use model access according to your provider's terms. Sharing a personal
subscription with other people may violate them; node roles do not establish
contractual permission.

## Containers and credentials

Worker images are built directly with Nix, with immutable pins, non-root users, a
read-only root, dropped capabilities and resource limits. There is no host
fallback when a worker prerequisite fails. Credential logins are mounted
read-only and checked by metadata only; a sandbox probe with an empty canary
credential must pass before the real login is mounted. See the
[threat model](THREAT-MODEL.md) and [sandbox notes](../hosting/worker-sandbox.md).

## Lenses, tools and knowledge

A lens composes documents, tool descriptors and instructions into a run's
context; it grants no authority. The resolver rejects cycles, unknown references
and selected context over 48,000 characters, and each run stores the revisions it
resolved ([lenses](LENSES.md)). Tool registration is discovery metadata, not a
grant; brokers enforce node and actor permissions on every call.

Agents can use [solvers](SOLVERS.md), [planning tools](PLANNING-TOOLS.md),
[workspace commands](COMMAND-JOBS.md), [persistent services](PERSISTENT-TOOLS.md),
[ports](PORTS.md), [scoped web access](WEB-ACCESS.md) and
[tool packages](TOOL-PACKAGES.md). [Knowledge](KNOWLEDGE-TOOLS.md) is a set of
Markdown notes plus claims that only the operator can admit.

Managed agents ask consequential questions with `control_design_ask` (2–4
options, optional recommendation). The task pauses at a checkpoint and resumes
when answered; a recommendation is never consent.

## Artifacts

Published snapshots use immutable manifests with paths, sizes, content types and
hashes verified on read and restore: at most 512 files, 2 MiB per file and 16 MiB
per snapshot. Private, harness and generated files are excluded. These bounds do
not limit live workspace writes. Previews escape HTML; diffs show observed
changes, not permission to apply them. Files and chat output never become
accepted knowledge automatically.

## Collaboration and access

Rooms link chats for scoped coordination, claims and notifications
([room notifications](ROOM-NOTIFICATIONS.md)). Persistent bots wake on watched
events within approved budgets ([watches](WATCHES.md)). Discord access is bound
to numeric IDs ([setup](setup.md#discord)).

The website binds to loopback, validates Host/Origin and requires an operator
session; only `/health` is public. Federation is an offline slice: peer pinning,
signed lens exports and quarantined imports, with no live listener or remote
execution ([federation](FEDERATION.md)).
