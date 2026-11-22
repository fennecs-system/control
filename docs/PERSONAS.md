# Personas

A persona is a named, versioned composition of pieces Control already has. It
adds no supervisor, grant or engine:

| Part | What it is | Built from |
| --- | --- | --- |
| **Purpose** (SOUL) | Why it exists: a short instruction, shown first in its prompt | Inline text (at most 600 characters) or a knowledge document authored as `SOUL.md` (at most 2,000) |
| **DNA** | The genes its runs resolve | [Lenses](LENSES.md) |
| **Standing** | What it may see, whom it may steer, whom it answers to, what it may act on | The host's existing role rules, now read in one place |
| **Body** | Its default model | A model selector such as `claude:opus` or `local:NAME`; empty inherits |
| **Behaviours** | Opt-in standing orders | Bot timers and room wakes, watches, observations, score probes |

Personas live in a library beside DNA (**Personas** in the navigation,
`/personas`). Like genes, a save appends a revision; earlier revisions stay
readable, and a node adds new built-ins without replacing local edits. Runs
capture the revision they start with (purpose resolved, `SOUL.md` copied in), so
editing a persona never changes running work. Continuations of a run keep its
capture; spawned children get their own persona, never a copy of the parent's.

## Built-in personas

| Persona | Purpose | DNA | Sees | Steers | Answers to | Acts | Body |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `lead` | Plan, assign, unblock, keep turns short; delegate long work | `swarm` | own team | its team | operator | full worker tools | inherits (set a strong hosted model) |
| `worker` | Complete claimed work; coordinate with peers; report to the lead | `swarm` | own team | its team | lead | full worker tools | inherits (set a local model if wanted) |
| `advisor` | Catch the lead's gaps; one concise evidence note | `advisor` | own team | its lead (notes) | lead | read-only judge set | inherits |
| `acceptance` | Compare what the team calls finished with what was asked; note the member whose claim falls short | `acceptance` | own team | its lead (notes) | lead | read-only judge set | inherits |
| `antibody` | Patrol every team for pathologies; note leads; escalate | `antibody` | the node | other teams' leads (notes) | operator | read-only judge set | inherits (a local model once reliable) |
| `judge` | Independent review; report defects with evidence | none | own team | no one | lead | read-only judge set | inherits |

Built-in bodies are empty because a node's model names differ; set them on the
Personas page (for example `claude:opus` for `lead`, `local:NAME` for
`worker`). A body must name a configured model; an unknown one refuses the spawn
rather than falling back.

## Using a persona

- **Spawn:** `control_agent_spawn {"objectives":[…],"persona":"worker"}`. The
  persona supplies DNA, standing, body and behaviours. Explicit `lenses` replace
  its DNA and `model` overrides its body; both are recorded on the child. The
  standing decides the role: read-only personas are judges (`role`, if given,
  must agree). There is no separate `control_bot_spawn`: spawn the persona, then
  `control_bot_request` (or a schedule behaviour) makes it a bot.
  The operator approves `control_bot_request`. With the node's automatic
  approval in Auto ([ports](PORTS.md#automatic-approval)), the request is
  approved at once and recorded with the policy as decider when all of these hold:
  - the requester's top-level chat is an authorized operator's task;
  - the tree is within its spawn limit (`max_agents_per_root`);
  - the tree runs fewer than 8 bots, and the node fewer than 32;
  - the chat is not a read-only advisor or antibody.

  Anything else asks. `/bot off` stops the bot as usual. Bot requests filed by
  a schedule behaviour at spawn time always ask.
- **Root DM:** `/persona lead` in the terminal, `!persona lead` on Discord, or
  `control.chat.persona`. The persona's genes become the DM's DNA (the gene
  picker can still change them) and each message captures the persona. Its body
  applies when the DM has no `/model` of its own. Read-only personas are only
  spawned. `/personas` lists the library; `/persona none` clears.
- **Operator tools:** `control.personas.list`, `control.persona.read {id,
  revision?}`, `control.chat.persona {id, persona|null}`.

## Standing: one place, host-enforced

`crate::personas::of(run)` is the only reader of a run's standing. Judge tool
policy, deferred tool listings, who `control_agent_message` may reach, intel
brief scope, operator notices and the advisor/antibody bot mode all ask it.

A run without a captured persona (every run before personas, and every spawn
without `persona`) gets the shipped built-in for its position: `lead` at the
root, `worker` below it, and for read-only judges `antibody` (approved on the
node topic), `advisor` (approved on its team topic) or `judge`. An
`acceptance` check is always a captured persona: its DNA decides that it hears
only its team's completion. Tests prove the
resulting policy is identical to the earlier scattered checks.

A standing is a profile, not a grant. Every existing gate still applies:

- Observers need their opt-in gene on the root: `advisor` (or `acceptance`) for team observers,
  `antibody` on an operator-owned root for node observers, and the root's
  advisor allowance. Operator bot approval still decides whether one runs, and
  only an approved observer may send notes or notices.
- Node visibility and lead steering exist only for read-only personas; acting
  personas steer their team (the peer rule) or no one. A persona answering to
  its lead cannot send operator notices; one answering to the operator still
  needs the `operator.notify` gene and a top-level chat (or antibody approval).
- `acts: {"deny": [...]}` removes tools from the full set (hidden from listings
  and refused on call). Tree position, depth limits, budgets and actor checks
  are unchanged.

## Behaviours

Behaviours are a declarative list in the persona, composed only from existing
primitives, applied when the persona is instantiated and visible in its capture:

| Kind | Applied as | When |
| --- | --- | --- |
| `{"kind":"schedule","every_minutes":N}` | A pending bot request with that timer (the operator approves); also the default `every_minutes` of a later `control_bot_request` for it | Spawn |
| `{"kind":"room"}` | A pending bot request that wakes on room posts (team room linked) | Spawn |
| `{"kind":"watch","name":…,"command":…,"salience":…}` | A standing order to install the routine with `control_watch_start` | First turn, in a service it owns or was granted |
| `{"kind":"observe","command":…}` | A standing order for `control_observe_set` | First turn |
| `{"kind":"score","name":…,"command":…}` | A standing order for `control_score_probe` | First turn |

Service-bound behaviours are rendered with exact tool arguments under the
persona's purpose, because a watch, observation or probe needs a running
service the agent owns; the agent installs them through the ordinary tools and
host checks. Read-only personas take no service behaviours, and an advisor takes
no schedule (an antibody may patrol). At most 8 behaviours.

This is the slot later work plugs into: cron-style schedules and recipes
([W1/W2](SWARM-ROADMAP.md#w1-cron-style-schedules)), behaviour trees and state
machines as the routine language, observation sources and salience filters
([H1](SWARM-ROADMAP.md#h1-layered-control)), and host-installed routines.

## Prompt

A captured persona's first instruction reads:

```text
Persona worker@2 (Worker): Complete the work you claimed. …
Standing (host-enforced): sees its own team; steers its descendants and fellow workers; answers to its lead; acts with full worker tools.
Persona behaviours (standing orders; …):
- routine restock: control_watch_start {name:"restock", command:"./restock.sh"}
```

It precedes the existing role framing and DNA, and stays byte-identical across a
chat's turns. Current user direction and enforced policy still take precedence.
