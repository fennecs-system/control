# Demonstration goals

Planned acceptance scenarios, 2026-10-30. These should demonstrate breadth using
the same tools, resource grants, lenses, personas and supervisor as normal work.
They do not add a website builder, price-tracker subsystem or game scheduler to
Control's core. See the [roadmap](../roadmap.md) for priorities and open gates.

Control and authored integrations remain Rust/Haskell/Lean/WASM/Nix. Non-Rust
third-party tools may run only inside tool or agent containers, with pinned
dependencies and explicit resource/network grants. A Rust adapter can call an
isolated upstream tool; no authored JavaScript integration or host Node service.
Keep base images immutable and generated tools in private writable state.

## Foundation: Control improves Control

Use `/improve` for a small real change: discuss, edit isolated source, invoke a
declared build/test environment, follow long-running command output and retain
verification alongside a reviewable diff. Review and application to the checkout
remain explicit. Repeat with a follow-up and a server restart before relying on
this loop for larger work. This is the first practical target, not a fifth
special-purpose framework.

## 1. Build a non-trivial website

An agent builds a useful website with multiple routes/views, persisted data,
input validation and tests. The owner opens a running preview, requests a
meaningful change and inspects the resulting diff and verification. Reconnect to
the chat and restart Control without losing source or the published checkpoint.
Verify actual behavior with the browser tool, not only a successful compilation.

Needed capabilities: a reproducible project build environment with dependencies,
commands longer than the current short Claude shell deadline, usable logs and
browser checks. Current preview mounts are read-only and temporary files vanish
with the container; persistent application data needs a separately scoped resource.
Do not quietly make the source or host filesystem writable to solve that need.

## 2. Browser agents and a phone-price history tool

Give each agent a different authorized website and a comparable phone variant.
Agents use a real browser session to observe offers, and build a tool with a
Rust/WASM view reachable inside Control. It shows price over time and links each
point back to its source. This exercises organized collaboration, structured data
and a tool-provided interactive view; it is not another hardcoded dashboard tab.

- Start with controlled browser fixtures, then a small set of public sites.
  Agents can propose extraction code within their isolated tool environments.
- Store observations as structured data: source URL, observation time, product
  variant, currency, amount, availability and bounded supporting evidence. Keep
  missing values, failed visits and non-comparable offers explicit; no invented
  prices or silent conversion between variants/currencies.
- Run at least two collection rounds and show each site's independent results.
  Retry/restart must not duplicate an acknowledged observation. Stored captures
  can verify extraction and rendering without repeatedly calling live websites.
- Keep routine scheduling, validation and aggregation deterministic. Use agents
  for website differences and changed layouts; do not require model calls for
  every unchanged price sample. Results belong in a dataset, not a growing pile
  of prose in the shared knowledge base.
- Publish the generated tool/view through the normal tool and artifact/resource
  contracts, with the appropriate browser-origin isolation and bounded output.

Gaps: interactive network-granted browser sessions, bounded agent delegation and
parallelism, durable collection triggers, a shared observation store, and a
versioned tool/view publication contract. Demonstrate manual collection rounds
before introducing unattended schedules.

## 3. Five agents build together in Minecraft

Start a private test world with five separately identified Sol-backed agents;
model choice remains configurable rather than fixed into the integration. Each
has its own persona, tool environment and bounded budget. They can join, observe,
act, write helper tools, and coordinate through an authorized room. Shared tools
are versioned proposals with explicit publication/adoption; one agent cannot
silently replace another's trusted tool. Stable discoveries can become admitted
knowledge, while ordinary chatter stays in the room.

Separate the persistent game connection from replaceable model runs. Put movement,
pathfinding, observation filtering and action cancellation in deterministic tools;
do not spend a model turn on every game tick or placed block. Capture completed,
failed and uncertain actions distinctly. World observations are authoritative for
game state; a chat claim alone does not prove a delivery or completed structure.

Progress from one connected bot, to two coordinating a small task, to all five
building a common structure. Restart one worker, cancel an action and demonstrate
that peers keep their identities and project context. The server/world is a
separate granted resource; a game command grants no host, operator or Discord
authority. Use the same bounded supervisor rather than a second game runtime.

The [YC experiment](https://labs.ycombinator.com/projects/games/index.html#minecraft)
uses a Pi-based harness and Mineflayer tools. Its
[open-source harness](https://github.com/yc-software/minecraft-agent-harness)
is a useful reference for observation/action and communication boundaries.
[Mineflayer](https://github.com/PrismarineJS/mineflayer) is a JavaScript/Node library:
under the container exception it can be pinned as an upstream tool dependency,
while Control's adapter and tool protocol remain Rust. Reusing that dependency
does not require adopting Pi or writing a JavaScript Control integration.

## 4. The owner joins and steers the team

The owner joins the same world and asks the team to build a castle. An agent asks
through the existing durable question service: stone brick or cobblestone? The
question reaches the owner's confirmed Discord DM with a task link. An answer
returns to the exact question/task, updates the team's shared plan and lets work
continue; unrelated agents need not pause. Important decisions can originate from
the game task without creating a separate Discord-only conversation.

Demonstrate steering the plan midway through construction, a delayed answer,
duplicate delivery and a bridge/server restart. Pending answers remain pending;
in-game messages cannot impersonate the owner's Discord identity. Keep questions
selective: the team should handle ordinary execution details itself.

Gaps: persistent bot/task identity, event delivery to active agents, bounded
parallel execution, a Minecraft tool adapter and owner-to-task routing. Existing
rooms, human `/spawn` and structured Discord questions supply parts of this, but
do not yet constitute the complete demonstration.
