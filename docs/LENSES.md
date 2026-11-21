# Lenses and bounded investigation

A lens is versioned context: instructions, captured knowledge documents,
descriptive tool hints and other lenses. It is not an agent identity or a grant.

**User-facing names.** The UI calls the library **DNA** and each lens a
**gene**: genes include other genes, a DM's selected set is its DNA (genome),
and revisions are versions. Users experience a lens as a bundle of tools,
documents and instructions that spawned workers inherit, not as a view into
something. Internally lenses remain pure lens transforms, and code, state
fields, tool names and APIs (`control.chat.lenses`, `control.lenses.*`) keep
the `lens` name. Likewise a 1:1 conversation with an agent is shown as a **DM**
(distinct from **Rooms**), while APIs stay `control.chat.*`.

User-facing URLs follow the visible names: the library is at `/dna`, the DM
list at `/dms` and a DM at `/dm/ID` (with sub-pages such as
`/dm/ID/transcript`, `/dm/ID/project` and `/dm/ID/compaction`). The earlier
`/lenses[/...]`, `/chats[/...]`, `/chat` and `/chat/ID[/...]` paths, and
`/?view=lenses`, keep working: reads get a 301 and other methods a 308 to the
new path with the same tail and query, so bookmarks, sent Discord links and
forms rendered before the rename still work.

Use **DNA** to create and edit genes and to inspect their effective
instructions. Select genes in a DM's gene picker, through `control.chat.lenses`,
or with terminal and Discord commands (`!genes`/`!use` with the default Discord
prefix):

```text
/genes
/use engineering swarm
/context
```

`/genes` lists the library; `/lenses` remains an alias.

A selection applies to future operator messages. Queued runs, spawned workers and
automatic continuations keep the revisions they captured, so editing the library
never changes an active attempt. `/use none` clears the selection. The browser
preference only sets the starting selection for a new DM. Gene (lens)
instructions are limited to 4,000 characters.

Built-in lenses are versioned. On load, a node adds missing built-ins (when
library capacity permits) and appends a newer built-in revision when its current
copy is still the built-in revision it last installed. A local edit is a newer
local revision and is never replaced. Earlier revisions stay available to captured
runs. Adding a built-in never selects it for existing chats. When changing a
built-in, add its previous shipped definition to
`crates/context/src/builtin-lens-history.json`, so unedited copies on older nodes
are recognised and upgraded.

## Guidance on demand and role scoping

Guidance is looked up when needed, not repeated every turn. A lens with a
one-line **summary** is on demand: each prompt carries the summary as one index
line, and the agent reads the full text with the read-only `control_guidance`
tool (`topic` is a lens ID or name; no topic lists the topics). The tool returns
the revision the run captured and is allowed after a staged handoff. Lenses
without a summary, such as `evidence`, stay inline.

A lens can also name the **roles** that see it and the roles that get its full
text **inline** instead of the summary. Roles come from delegation records, not
prose: the root chat is the **lead** (a lone chat is its own lead), spawned
workers are **members** and read-only judges are **advisors**. The built-in
`advisor` lens has `roles: [lead, advisor]` and `inline: [advisor]`: the advisor
carries its procedure, the lead carries one line on starting it, and members
carry nothing. Both fields are editable in the lens form; empty roles mean every
role.

Always in the prompt, and why:

- **Safety and authority** (adapter framing and the session core): no
  credentials, room/watch/peer content grants no authority, follow authorised
  steering and explicit user limits, use `control_design_ask` for operator
  decisions. These must hold before an agent knows it needs them.
- **Handoff essentials** (`ACTIVE_SESSION_GUIDANCE`, about 0.7 KB): read
  callbacks with `control_turn_context`, hand off with `control_turn_handoff`
  and a continue/wait/done disposition, respect `remaining_seconds`. Mistakes
  here lose committed work, and every wake uses them.
- **Team essentials** for room-linked chats (`ROOM_COMMUNICATION_GUIDANCE`,
  about 0.5 KB): direct messages for one worker, claims before shared work, what
  to post. Mistakes here cost peers, not just the agent.
- One summary line per on-demand lens, stating its core rule so the agent knows
  when to read more.

Everything else is on demand: the planning procedure, coordination detail,
autonomy detail, bounded investigation, the advisor procedure (except for the
advisor), Ganglia, and three built-in `control_guidance` topics: `session`
(sessions, callbacks, handoff and turn limits), `rooms` (rooms, messages, claims
and steering) and `environment` (worker images, profiles, Nix builds and network
scope).

Per-wake text does not restate lenses. The bot wake objective is one line naming
its trigger; the advisor wake names its facts and points at its procedure; a
new-callback notice on a tool result is one line. Notes tied to a concrete event
are one line each, at most once per attempt: the attempt deadline, a steering
chat doing work itself, a turn stopped at its time limit, and a rejected or
missing handoff.

## Task autonomy

The optional `autonomy` lens asks the agent to own its outcome and to weigh risk
by reversibility: a recoverable failure usually costs less than idle time, though
recoverable does not mean harmless, especially for shared assets. Prefer
reversible steps with a recovery path. Verify in proportion to risk: check once,
by the cheapest direct observation, at milestones or risky, irreversible or
disputed steps. Never claim an unobserved outcome. Uncertainty alone calls for
investigation with existing tools, not stopping.

Guards bound damage (stop, back off or roll back at a stated limit); they should
not prevent an attempt. A self-authored guard is a hypothesis about risk:
investigate its rationale instead of treating every rejection as a reason to wait.
Host-enforced permissions, mandatory safety constraints, cancellation and explicit
user limits stay binding. The lens grants no broader access and no consequential
action beyond the task's mandate.

Repeated actions on an external system belong in a long-lived workload: a bounded
routine that works toward a target on its own (for example, process N items,
deliver them and notify). The routine sequences steps, retries and handles events,
escalating only milestones, failures and situations it cannot handle. The model
decides what to do, when and by whom, and re-plans from those escalations. This
matters because one job per tool call plus status polling runs at about one action
per model round trip or slower.

A workload needs a current job and a path to the next authorized job, not only a
connection or health monitor. A finished job means notify and take the next one;
a setback means recover, not quit. Test the notification path before waiting.
Notify on completion, actionable blockers or transitions that open useful work,
with the job ID and outcome; deduplicate and keep routine diagnostics out. Assign
work through the workload's existing interface instead of restarting a healthy
process per action. Build only the tooling the current goal needs.

Repeated unchanged cycles are not progress: after two routine cycles without
milestone progress, report the blocker, take unblocked work or ask for a new
assignment. A quiet healthy download, calculation, service or dependency wait
needs no periodic model wake or invented deadline.

## Planning procedure

Select **Planning procedure** (`/use planning`) for a broad outcome, or **Swarm**
(`/use swarm`) to combine it with coordination, autonomy and investigation. The
lens library is the procedure mechanism: there is no separate planner API, task
queue, scheduler or provider-specific skill loader.

Keep a short rolling backlog of milestones in the task's notes or shared room,
and reference it instead of repeating it. Slice by outcome, not by role or
resource: a milestone is a small piece of the finished result that one member
can complete alone in a few turns, getting its own common inputs. Give each a
done-condition that is an observable outcome cheap to check (a count, size or
state), and check it once when claiming it done. Keep more open milestones than
members so whoever finishes pulls the next; a milestone still open after several
turns is too big, so split it or have a peer join it. A team that sliced by role
(one builder, one supplier) finished nothing: builders waited on the supplier
for material they could have fetched themselves.

Build the smallest tool the current milestone needs, test it by reaching the
done-condition, and extend it only when the next milestone needs more. Start with
a useful end-to-end result, then expand: for an integration, first one working
request, then error handling and recovery, then more operations. Connection
health alone is not the feature. Simple local tasks need no team plan.

## Coordination

The optional `coordination` lens (which includes `planning`) helps a team turn a
shared outcome into small, verifiable deliverables with accountable owners.
Planning has no single point of failure:

- Whoever steers a team posts the milestone backlog in the shared room before
  spawning or briefing workers, and briefs each worker with the shared goal and
  its first milestone, not a permanent role. A team whose lead posted only a role
  split, with no backlog, had nothing to claim.
- The backlog is shared state; any member may revise it. A worker that finishes
  a milestone claims the next. If the backlog is empty or stale, or the steering
  chat is quiet, the worker proposes the next milestone in the room and claims it.
- Claims, not permission or a central planner, prevent accidental duplicate
  work. Claim shared deliverables, not personal roles. The claimant stays
  accountable, but a peer may join a slow or stuck milestone without taking it
  over. For an uncertain problem, two members may deliberately try different
  approaches to one milestone, and a judge or whoever steers picks the result.
  Claims are advisory (see [Room notifications](ROOM-NOTIFICATIONS.md#claims))
  and only help when agents check them.

Dependencies:

- Minimise dependencies and distinguish prerequisites from optional precautions.
  Record a blocker's producer and release signal in notes and handoffs, check for
  circular waits, and do independent authorized work while waiting.
- Wait on a claim only for an output you cannot reasonably produce yourself. If
  an input takes about as long to make as to ask for, make it.
- Coordinate only over consumable or exclusive resources. Shared access paths,
  read-only checks and routine independent work need no claim, turn, token or
  permission; Control services accept concurrent commands. Teams that invented
  token-passing for a shared path spent most of their messages on the token.
- Check that a dependency's producer can act and is not waiting on you. If it
  cannot, hand over ownership or work around it; elapsed time alone does not
  justify duplicating healthy work. When you observe a condition a dependent is
  waiting on, tell it.
- When nobody can unblock the goal, or a step needs operator approval, ask with
  `control_design_ask` and options, and apply standing guidance to similar
  steps. A room post or note "awaiting direction" is not a question: nothing
  reaches the operator, and a claim still marked working fires no blocked or
  advisor signal.

Communication:

- Coordinate directly with affected peers. Post to the room only what others
  need (a milestone advanced, a handoff, a blocker raised or resolved, a reusable
  finding), briefly, stating the outcome without evidence paths. Routine outcomes
  stay in the agent's notes.
- When several members report one blocker or milestones stall, whoever steers
  splits stalled milestones smaller, moves members onto them or routes around
  the blocker, and escalates once. Refreshing the backlog or only asking owners
  to diagnose is not a response. An operator question opens with what is
  blocked, the impact and the requested decision.
- The steering chat stays responsive: it assigns goals, resolves consequential
  choices and unblocks, keeping turns short, and delegates implementation, long
  work and ongoing services to members peers can contact directly. A lead that
  does long work itself can hit its attempt time limit, lose unpublished work and
  leave the team unsteered. The host backs this with advisory notes on tool
  results (see [turn continuity](TURN-CONTINUITY.md#deadline-and-steering-notes))
  and a `timed_out` advisor fact.
- Check existing task tools and peer findings before reimplementing a common
  capability. Share a reusable finding (a working tool with its invocation and
  version, a method or a proof step) once, where the team will find it: existing
  artifacts, an explicitly shared service or knowledge review (proposals are not
  admitted automatically). Reuse does not
  require sharing a process or mutable state, and existing access controls still
  apply.

**Swarm** (`/use swarm`) includes `planning`, `coordination`, `autonomy` and
`investigation`. Select it before launching a team: existing chats and running
workers are not opted in when a preset is added, and spawned work keeps captured
revisions. This is versioned guidance over existing tools, not a planner,
permission grant or guarantee against deadlock. Evaluate an improvement in a
separate opt-in run (useful actions, dependency releases, repeated messages,
interventions) before claiming it.

## Bounded investigation

The optional `investigation` lens includes `evidence` and is prompt guidance
over existing tools, not a scheduler, experiment executor or host-enforced
counter. When a failed action or an unchanged observation blocks a plan:

- Do not repeat the identical action; a status query or the same rejected retry
  is not a new try. Check notes for earlier failures, then change one relevant
  thing per try.
- After about three distinct tries at one approach, switch to a materially
  different approach or ask a peer. Renaming it, changing a minor parameter or
  handing it to another worker is not a new approach.
- Prefer an authorized, reversible alternative that preserves the requested
  outcome. Escalate once through the design-question path when none remains, or
  before changing the goal, broadening access, exceeding the user's budget or
  acting outside the mandate; otherwise do independent useful work or wait on a
  real dependency.

Earlier revisions asked for a written hypothesis, expected observation and
disconfirming result per probe. Models followed that literally, writing one
micro-probe per turn instead of acting, so the record-keeping was dropped.

## Team advisor

The team advisor catches gaps the team could miss and answers members'
questions about method. It is not a supervisor, planner, relay or churn monitor,
and it holds no authority.

**Strictly opt-in.** Select **Team advisor** on the root chat (tick it, or
`/use advisor`), usually alongside `swarm`, which does not include it. The lens
offers the `team.advisor` tool hint; unless the root chat's lenses offer it, no
advisor can be spawned or requested and Control does not compute the team's
lifecycle topic. This is an availability gate, not authority: the operator's bot
approval still decides whether an advisor runs, and host checks bound what it can
do. Unticking the lens stops its facts.

The advisor is a read-only **judge** (see [delegation](DELEGATION.md)). The lens
is its procedure and voice; policy comes from the judge role and the operator's
bot approval.

1. The lead starts it with `control_agent_spawn` and `role: "judge"`, with one
   objective naming the shared outcome. It gets no resources. It uses the root
   task's advisor allowance (`max_advisors_per_root` in `execution.json`,
   default 1, range 0–8), which is separate from the root's one blocking judge
   review. `persona: "advisor"` instead of the role spawns the built-in
   [advisor persona](PERSONAS.md): the same judge, with the `advisor` gene as
   its DNA and its purpose first in its prompt.
2. The lead requests it with `control_bot_request` (no interval). The operator
   approves it like any bot, or types `/bot on` in its chat. The lens or a
   request alone runs nothing.
3. The approved judge sleeps on its task tree's lifecycle topic,
   `task:<root chat>`. Control publishes host facts there, edge-triggered from
   records reconciliation already reads. Facts are gaps, not churn:
   - the team went quiet, with no member turn running or queued;
   - a member's latest turns failed or were blocked several times in a row
     (`advisor_failure_streak`, default 3; interrupted and cancelled turns
     neither extend nor break a streak), reported once per streak;
   - a watched restart loop was retired after repeated exits;
   - a claim was abandoned by a lost owner;
   - a member's claim became blocked. The fact names the claim it is blocked on
     (`blocked_on`) with that claim's owner and status (`working`, `blocked`,
     `done`, `abandoned`, `released` or `unclaimed`). Members blocked on the
     same claim are summarised in one entry. A blocked state is reported once
     per claim incarnation; becoming blocked again after working is a new fact;
   - two or more members report a similar blocker. Blocked claims' notes and the
     latest result of members on a failure streak are compared as sets of
     lower-cased words of three or more letters (words with digits, such as IDs,
     ports and times, are ignored; at least two shared words and Jaccard ≥ 0.6;
     no embeddings). One entry lists the members and one reason. A group is
     reported when a member joins it;
   - the team is busy but stalled: `advisor_stall_turns` (default 24) member
     turns settled while members held working or blocked claims and none
     advanced. A claim advances when it becomes done or resumes working after
     being blocked (the host records `advanced_at`); acquiring, re-reporting or
     posting is not progress. A stall is reported once; an advance, or open
     claims appearing or clearing, restarts the window. The window counts turns,
     not time. Teams that do not use claims never stall;
   - an idle claim holder: a member holds a `working` claim and has no turn
     running or queued and no running watch while a third of
     `advisor_stall_turns` (default 8, at least 4) other team turns settled since
     its own latest turn. Unlike a quiet team, peers are still busy, so only
     this member sits with nothing that will wake it. The fact names the member
     and one such claim; it is reported once per idle spell, and the member's
     next turn re-arms it; A persistent member
     that holds no live claim at all while its team still has working claims is
     reported the same way, as an idle member (one-off helpers, the lead and
     observers are not);
   - a bottleneck: one live (working or blocked) claim that the claims of
     `advisor_bottleneck_members` (default 2) or more members other than its
     owner wait on, directly or transitively through `blocked_on`. The fact
     names the claim, its owner and status, what it is blocked on, any circular
     wait, its age in claim updates since acquired and since last advanced, and
     the waiting members and claims. It is reported once per claim incarnation
     and dependent set, and again when that set changes. The dependency graph is
     walked only when a member claim changes;
   - a member cannot checkpoint: more than `bot_recovery_continues` (default 2)
     of its turns ended without an accepted handoff since its last accepted one.
     The fact names the member, the count and the host's last handoff rejection,
     if any; it is reported once per streak;
   - a turn timed out: a member's or the lead's latest turn reached its attempt
     time limit and was stopped before it checkpointed (`timed_out`). It is
     reported once until that chat's next turn settles otherwise. The advisor
     suggests splitting the work or, for the lead, delegating it;
   - a pathology (see [antibodies](#antibodies)): harness churn, a repetition
     loop, polling or cost burn.

   Apart from a timeout, a single failed, interrupted or cancelled turn
   (provider errors included) and a single failed watch exit are never facts.
   Facts coalesce into one wake under the bot's 30-second cooldown and 60 turns
   per hour. There is no clock rule: elapsed time is never a fact.
4. On waking, the advisor reads the facts and `control_room_read` (participants,
   posts and claims with `owner_idle`/`abandoned` flags). If there is a gap, it
   sends the lead's steering chat one concise note with `control_agent_message`:
   the evidence and a suggested step. For blocked members it checks whether
   they could quickly make the input themselves or several wait on one
   producer, and suggests producing it, reassigning the blocking claim or an
   alternative. For a bottleneck it suggests adding members, splitting it or
   re-routing dependents. It sends nothing when evidence is unchanged.

Issues rise through the team, not through the lead. The advisor notes the
member a fact is about; it notes the lead only for what members cannot settle
themselves (the plan, a reassignment, a blocker spanning several members, or a
member that did not act on an earlier note). A note is retained peer evidence
with source `advisor-note`, shown as "Advisor note from …" in its own box. An
active recipient gets one callback hint at its next tool response; one running
as an approved bot wakes within its own budget; an idle one is woken by the note
at once. The advisor never posts to rooms.

Members ask the advisor directly with `control_agent_message` (no services)
instead of spawning a reviewer; the question wakes it, and it answers that
member with one note. Judges that are not approved advisors take no messages.

The host refuses notes from judges that are not approved advisors, notes to any
chat outside the advised team, notes to other judges, notes that share services,
and a note identical to one already delivered. A note to a lead whose latest turn failed or
was cancelled is refused by the peer rule that messages cannot restart stopped
work, unless the lead is itself an approved persistent bot whose turn failed or
was interrupted: the note then waits for its next supervised wake.

Loop safety is structural. Judge turns, retained notes and turns woken only by
advisor notes (the lead's included) are not team activity, so the advisor cannot
re-trigger itself. Stopping the advisor (`/bot off`) drops its team snapshot; a
later advisor reports only facts after its own subscription. Nodes without an
opted-in advisor or antibody do no lifecycle work.

## Antibodies

Antibodies are the node's immune system: opt-in read-only observers that patrol
every team on the node (counter-intel duty), tag pathologies and recruit the
response from the team's lead or the operator. They never do the work, take
over, message members, post to rooms, claim work or start services. Where an
advisor serves one team, an antibody watches all of them.

**Strictly opt-in, operator-owned.** Select the **Antibodies** gene
(`/use antibody`) on a root chat the local operator owns. The lens offers `node.antibody`,
`intel.brief` and `operator.notify`; without it nothing is computed. As with
the advisor, the lead spawns a judge (`control_agent_spawn` with `role:
"judge"`, from the root's advisor allowance) and asks for it with
`control_bot_request`; the operator's approval (or `/bot on` in its chat) runs
it. An antibody may take a patrol timer (`every_minutes`); an advisor may not.
`persona: "antibody"` spawns the built-in [antibody persona](PERSONAS.md),
whose captured standing (node visibility) selects the antibody mode even in a
tree that also offers an advisor; the antibody gene gate still applies.
A root owned by a Discord user cannot run one, because an antibody reads every
team on the node.

**What wakes it.** The approved judge sleeps on the node topic. Each reconcile
pass observes every team (a root chat with spawned members, except antibody
trees) in the same single pass the advisor uses, and offers one coalesced event
of the facts that suggest a team is off course: repeated failures, retired
restart loops, stalls, idle claim holders, shared blockers, bottlenecks,
timeouts, members that cannot checkpoint, and four pathologies:

- **harness churn**: `pathology_churn_commands` (default 12) file- or
  tool-building commands (redirects into files, `tee`, in-place edits,
  `chmod +x`, patch application, builders and package managers) since the
  team's claims last advanced or a score probe last rose. Building a generic
  harness instead of placing the stone wall looks like this;
- **repetition loop**: one member's same command (whitespace collapsed, digits
  folded) failed `pathology_repeat_failures` (default 3) times among its
  retained commands;
- **polling**: `pathology_poll_commands` (default 6) of one member's retained
  commands only read status (the same classifier as the polling nudge); a
  routine (`control_watch_start`) should report instead;
- **cost burn**: `pathology_burn_tokens` (default 2,000,000) fresh tokens (uncached
  input plus output; cached input is not counted) spent by the
  team's turns since its claims last advanced or a score last rose.

Inputs are records the host already keeps: each service's retained finished
commands (up to 16 per service), run token counts, claims and score probe
samples. Commands are classified only when a team's retained set changes. Each
fact is reported once and again only after it clears; history before the
antibody's subscription is not reported. Quiet teams, single blocked claims and
abandoned claims are ordinary coordination for the lead or advisor and do not
wake an antibody. Stopping the last antibody drops the snapshots of teams no
advisor watches.

**What it does.** It reads the facts and `control_intel_brief` (one team with
`chat_id`, or the whole node), decides whether a team is working against its
goal, and sends that team's lead one note with `control_agent_message`. Notes
are allowed only to another team's steering chat (a top-level chat with
members), never to members or its own tree, carry no services, and arrive as an
"Antibody note" that the lead verifies like advisor evidence. If the lead is not
fixing it, or the operator is needed, it sends one `control_operator_notify`.

**Operator notices.** `control_operator_notify` is a small generic tool: an
approved antibody, or the lead of a tree whose lenses offer `operator.notify`,
records a short notice (at most 600 characters, optionally naming a team). It
is rate-limited node-wide (`operator_notices_per_hour`, default 4; 0 disables
notices) and an identical notice is refused for a day. Notices appear for a day
as `pending_notices` in `control.requests.list`, so channels forward each once
(Discord DMs, kind `notice`); replying steers the tree that sent it. No decision
is requested.

**Intel briefs.** `control_intel_brief` (worker) and `control.intel.brief`
(operator) render a Markdown brief of the last 24 hours: per active team its
goal, members, model turns, claims done and open, score probe changes, tokens,
observed pathologies (from the advisor/antibody snapshots, or "not observed")
and requests waiting on the operator. Untrusted text is made Markdown-inert. A
worker reads its own team; an approved antibody reads any team or the node.
`save: true` on the operator call writes `intel/brief-DATE.md` under the state
directory. Channels with `"brief": true` deliver the node brief once a day after
`brief_hour` (UTC); see [setup](setup.md).

## Ganglia

**Ganglia** (`/use ganglia`): reflexes and routines run locally; turns are
strategy, as an octopus's arms act through local ganglia while its brain chooses
where to go. Use it for long-running work on systems that change faster than a
turn, not one-off tasks or ordinary coding (the autonomy routine rule covers
those). Examples: a game agent or robot (reflexes keep it safe, skills move or
build), a long scrape (reflexes back off on rate limits, skills crawl a section),
price monitoring (reflexes retry and alert on outages, skills watch a basket), or
a live service (reflexes restart and shed load, skills run maintenance). The lens
text itself is domain-independent.

Reuse first: when an admitted or available [tool package](TOOL-PACKAGES.md)
already provides reflexes or skills, use it and read its README and notification
contract. Build or extend a layer only for a gap a run has demonstrated, keep
every layer minimal, and keep new layers in one folder ready to save as a tool
package. Reflexes commit; skills are routines that carry a whole target to its done-condition, not a small
batch (including
timed sequences the model parameterises) that escalate only what they cannot
handle; turns set goals and re-plan. The lens adds no tool or authority.

Two failure modes shaped the text:

- **Jitter.** A reflex layer that re-decides every tick on the nearest threat,
  cancels routines for any distant trigger, acts once per reaction and flips
  mode at a threshold makes the agent stop and start constantly. Reflexes must
  commit: hold until the trigger stays clear, act continuously, weigh every
  active condition, preempt only for urgent ones, and pause rather than cancel
  routines.
- **Patching layers.** A member spent a 45-minute turn repeatedly patching
  movement code instead of doing the task. A layer that blocks the task is
  bypassed or removed, not patched again and again.

`swarm` does not include Ganglia, so runs with and without it can be compared.

## Tool packages

A lens can name operator-admitted [tool packages](TOOL-PACKAGES.md), such as
`mc263-client@1` (pinned) or `mc263-client` (the latest admitted version when a
message is sent). Runs capture the exact version and commit with the rest of the
context. Each attempt gets a private copy at `/workspace/.control-tools/NAME` and
can read the manifest with `control_package_read`. Runs without such a lens see
no packages, so leave it out when a run should rebuild a tool from scratch.

## Current boundaries

Tool hints contribute to context and catalogue discovery. They do not filter
callable worker tools, and deselecting a hint revokes no grant. The exceptions
are `team.advisor` and `node.antibody`, which decide whether the advisor or
antibody option exists for a task tree, and `operator.notify`, which a lead needs
to send operator notices (availability gates, not grants). Host operations enforce execution
authority independently. Captured documents are attributed evidence, not
instructions or accepted facts. Inspect the captured context to check what a
worker received.

**Deferred tool loading.** Tool lists are role-scoped like prompts. A worker
session's `tools/list` holds only its role's core schemas; every other tool it
may call appears in the prompt's deferred index, one terse line per tool family
(e.g. `- watch_status (inspect watches); watch_stop (stop one)`).

- Core sets are catalogue metadata: each protocol name in
  `control_context::TOOLS` carries its core roles, index phrase (at most eight
  words), judge permission and optional tight description (`Mcp { core, phrase,
  judge, brief }`), so a new tool is registered once. The dotted/underscore name
  maps and judge allow-lists in the worker broker derive from the same entries.
- Members and leads load turn context and handoff; service start, status, exec,
  exec status and copy; watch start and events; room read, post and claim; agent
  message and spawn; bot request and status; guidance; and
  `control_workspace_run` where the backend uses Control's shell (Claude and
  local models). Leads add `control_room_plan` and `control_design_ask`. A judge
  loads guidance, room read, workspace start and status, child results and the
  shell; only an approved advisor may also call agent message.
- Always-loaded schemas (except a judge's) use the catalogue's tight
  description. All drop nested property descriptions and annotations and reduce
  a large optional property (such as the handoff payload or watch salience) to
  its type with a pointer to the search. They also drop the closed-object flag
  and type-sized bounds. The host validates every argument either way. Each
  role's listing is under 9.6 KB.
- `control_tool_search {names | query}` returns full schemas (at most 16 named
  or 5 ranked, 32 KB) of tools this session may call; without arguments it lists
  the deferred tools. A tool already in the session's listing comes back under
  `already_listed` as its name and index phrase, plus (when named) only the
  properties its listing reduced to a pointer. A local turn's result shows each
  found tool's name and one line; the schemas are in its function tools. It is a read for every role, including after a staged
  handoff.
- Codex and Claude call a deferred tool with `control_call {tool, arguments}`.
  The broker rewrites it into the direct call before any check, so authority,
  judge policy, the staged-handoff fence, design and delegation pauses, nudges,
  fixes and idempotency nonces are identical, and activity records the called
  tool's name. An unknown or operator-only name is refused with a search to run
  instead. Local turns load schemas natively ([LOCAL-MODELS.md](LOCAL-MODELS.md)).
- MCP `notifications/tools/list_changed` is not used: the room-mcp bridge only
  answers requests, and Codex's `enabled_tools` and Claude's `--allowedTools`
  are fixed at launch. Both list every catalogued worker name, so deferred tools
  stay callable.

Hiding a schema grants or revokes nothing: a deferred tool called directly by
name is dispatched exactly as a loaded one.
