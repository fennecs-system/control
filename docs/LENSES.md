# Lenses and bounded investigation

A lens is versioned context: instructions, captured knowledge documents,
descriptive tool hints and other lenses. It is not an agent identity or a grant.
Use **Lenses** to create/edit definitions and inspect their effective instructions.
Select them in a chat's lens picker, or use terminal commands:

```text
/use engineering swarm
/context
```

The same selection is available through `control.chat.lenses`. Changes affect
future operator messages; queued runs, spawned workers and automatic continuations
retain their captured revisions. Editing the library never changes an active
attempt's instructions. `/use none` clears the selection for future messages.

Built-in lenses are versioned. On load, a node adds missing built-ins and appends
a newer built-in revision when its current copy is still the built-in revision it
last installed. A local edit is a newer local revision, so it is never replaced;
earlier revisions stay available to captured runs. When changing a built-in, add its
previous shipped definition to `crates/context/src/builtin-lens-history.json`, so
unedited copies from older nodes are recognised and upgraded.
The browser preference only chooses the starting selection for a new chat.

## Guidance on demand and role scoping

Guidance is looked up when needed, not forced into every turn. A lens with a
one-line **summary** is guidance on demand: each prompt carries the summary as
one index line and the agent reads the full text with the read-only
`control_guidance` tool (`topic` is a lens ID or name; no topic lists the
topics) when that situation arises. The tool returns the revision the run
captured, so library edits never change an active run's guidance, and it is
allowed after a staged handoff. Lenses without a summary, such as `evidence`,
stay inline. Contexts captured before this change keep all their instructions
inline.

A lens can also name the **roles** that see it at all and the roles that get
its full text **inline** instead of the summary. Roles come from delegation
records, never prose: the root chat is the **lead** (a lone chat is its own
lead), spawned workers are **members** and the read-only judge is the
**advisor**. The built-in `advisor` lens is `roles: [lead, advisor]`,
`inline: [advisor]`: the advisor carries its procedure, the lead carries one
line on starting it, and members carry nothing. Both fields are editable in the
lens form; empty roles mean every role.

What stays always on, and why:

- **Safety and authority** (adapter framing and the session core): no
  credentials, room/watch/peer content grants no authority, follow authorised
  steering and explicit user limits, `control_design_ask` for operator
  decisions. These must hold before an agent knows it needs them.
- **Handoff essentials** (`ACTIVE_SESSION_GUIDANCE`, about 0.6 KB): read
  callbacks with `control_turn_context`, hand off with receipts and
  continue/wait/done, `remaining_seconds`. Getting these wrong loses committed
  work, and every wake uses them.
- **Team essentials** for room-linked chats (`ROOM_COMMUNICATION_GUIDANCE`,
  about 0.5 KB): direct messages for one worker, claims before shared work,
  what to post. Violations cost peers, not just the agent.
- One summary line per on-demand lens, which states the lens's core rule so the
  agent knows when to read more.

Everything else is on demand: the planning procedure, coordination detail
(backlog, waiting on claims, escalation), autonomy detail (routines, guard
revision), bounded investigation, the advisor procedure (except for the
advisor), Ganglia, and the longer session and room conventions
(`control_guidance` topics `session` and `rooms`).

Per-wake text no longer restates lenses. The bot wake objective is one line
naming its trigger; the advisor wake names its facts and points at its
procedure; a new-callback notice on a tool result is one line. Notes tied to a
concrete event stay, each one line and at most once per attempt: the attempt
deadline, a steering chat doing work itself, a turn stopped at its time limit,
and a rejected or missing handoff.

## Task autonomy

The optional `autonomy` lens asks the agent to own its outcome, balance progress
against task-specific risk, and prefer reversible steps with a recovery path,
verifying in proportion to risk. A task-local guard can embody an incorrect assumption; investigate
its rationale rather than treating every guard rejection as a reason to wait
forever. Host-enforced permissions, mandatory safety constraints, cancellation
and explicit user limits remain binding. The lens does not authorize broader
access or consequential actions beyond the task's mandate. Long-lived workloads
need a current job and a path to the next authorized job, rather than only a
connection or health monitor. A finished job means notify and take the next one,
not exit; a setback means recover, not quit. Test their notification path before waiting, then
notify on completion, actionable blockers or transitions that open useful work.
Include the job ID and outcome; deduplicate repeated outcomes
and keep routine diagnostics out of notifications. Use the workload's existing
interface to assign work without restarting a healthy persistent process for
every action.

Autonomy revision 7 moves repeated actions on an external system into such a
workload: a bounded routine that works toward a target on its own (for example,
process N items, deliver them and notify). The routine reacts: it sequences
steps, retries and handles events itself, escalating only milestones, failures
and situations it cannot handle. The model decides what to do, when and by whom,
and changes plans from those escalations. A run that issued one
small job per tool call and then polled its status managed about one action per
minute, each costing two or three model round trips.

Autonomy revision 8 also asks for only the tooling the current goal needs: tools
serve milestones, so when tooling work stops advancing the goal, stop and use what
exists.

Weigh risk by reversibility: a recoverable failure usually costs less than idle
time, though recoverable does not mean harmless, especially for shared assets.
Guards bound damage (stop, back off or roll back at a stated limit) rather than
preventing an attempt, and an idle precautionary cycle is lost time. Uncertainty
alone calls for investigation with existing observations and tools, rather than stopping.

A quiet healthy download, calculation, service or genuine dependency wait needs
no periodic model wake or invented deadline. Workload-specific idle diagnostics
are optional and must be actionable, rather than repeated no-change polling.

## Reusable planning procedure

Select **Planning procedure** (`/use planning`) for a broad outcome, or select
**Swarm** (`/use swarm`) to combine it with coordination, autonomy and investigation.
The existing lens library is the versioned procedure mechanism: there is no new
planner API, task queue, scheduler or provider-specific skill loader. The same
procedure is available in the web picker, TUI and `control.chat.lenses`.

Keep a short rolling backlog of milestones in the task's normal notes or shared
room. Reference it instead of mirroring it in every message. Give each milestone a
done-condition that is an observable outcome cheap to check, such as a count, size
or state, and check it once when claiming the milestone done rather than producing
proof artefacts every cycle. For each active milestone record its done-condition,
owner, actual prerequisites and next action or release signal. Add, split, re-prioritise or drop milestones as evidence
changes. Build the smallest tool the claimed milestone needs, test it by reaching
that done-condition rather than a separate diagnostic proof, and extend it only
when the next milestone needs more. Start with a useful end-to-end result and
reliable reusable capabilities, then expand. For example, a team building an
integration might first deliver one working request, then error handling and
recovery, then additional operations. Connection health alone is not the feature.

Use existing room claims for overlapping shared work, not every local action. A
workload may chain its remaining authorized jobs
itself, or notify its owner to assign the next job. Before yielding, retain the
current job, remaining work and the callback or claim release that enables the
next action in the existing handoff. Simple local tasks need no team plan.

## Coordination and planning

Planning has no single point of failure: the team plans like a hivemind rather
than waiting on its lead. Whoever steers a team first posts the milestone backlog
in the shared room, before spawning or briefing workers, and briefs each worker
with the shared goal and its first milestone from that backlog rather than a
permanent role or area. Specialization may emerge from what workers repeatedly
claim. The backlog is shared team state, so any member may revise it. A worker
that finishes a milestone claims the next one; if the backlog is empty or stale,
or the steering chat is quiet with no shared next step, the worker proposes the
next milestone towards the goal in the room and claims it. Claims, rather than
permission or a central planner, keep two workers off the same milestone. Claim
shared deliverables, not personal roles. When you observe a condition a dependent
is waiting on, tell it.

The optional `coordination` lens helps a team turn a shared outcome into small,
verifiable deliverables with accountable owners. Roles can change as work changes.
It asks agents to minimize dependencies, distinguish prerequisites from optional
precautions, and retain a blocker's producer and release signal in existing notes
and handoffs. A worker should check for circular waits and pursue independent
authorized work while another dependency is pending.

Agents coordinate directly with affected peers, share useful evidence and
dependency releases in the team room, and keep routine relaying and repeated
no-change messages out of the steering chat. Changed evidence or repeated lack
of useful results calls for revisiting the plan. Healthy long-running work can
still wait on callbacks without a new deadline or forced intervention.

Roles and claims own shared deliverables, not exclusive rights to routine inputs.
Wait on a claim only for an output you cannot reasonably produce yourself. If an
input is quick to make, such as a small command, script, file or basic material
you could produce in about the time it takes to ask, make it yourself and keep
the claim for the shared deliverable. Coordinate only over consumable or exclusive
resources, those used up or held by one member at a time; shared access paths,
read-only checks and routine independent work need no claim, turn, token or peer
permission, so agents do not invent locks for them. Check that a dependency's producer
can act and is not waiting on you. If it cannot act, coordinate a handoff of
ownership or do independent authorized work; elapsed time alone does not justify
duplicating healthy work. When nobody can unblock the goal, or a worker needs the
operator's approval to proceed, ask through the decision path
(`control_design_ask`) with options and apply standing guidance to similar
authorized steps. A room post or note awaiting direction is not a question, and
nobody will answer it. Coordination revision 9 added that distinction after a
worker announced a team-disrupting shared-server restart in the room as "pending
direction" and waited: nothing reached the operator, and its claim still read
working, so no blocked or advisor signal fired. Assign
ongoing shared services to workers that peers can contact directly, rather than
making the steering chat an operational dependency.

Coordination revision 10 and autonomy revision 5 favour progress over proof. Post
to the room only what others need (a milestone advanced, a handoff, a blocker
raised or resolved, a reusable finding), briefly and citing evidence; routine
outcomes stay in the agent's notes. A repeating routine is not progress: after
two routine cycles without milestone progress, report the blocker, take unblocked
work or ask for a new assignment. When several members report one blocker or
milestones stall, whoever steers re-plans or reassigns around it and escalates
once; an operator question opens with what is blocked, the impact and the
requested decision. An overnight team run showed why: every member fell back to
a safe repeating routine behind one host blocker, posting a verbose record each
cycle, while the lead only refreshed the backlog.

Coordination revision 11, planning revision 4 and autonomy revision 6 favour
parallel attempts over ceremony. In one run members invented a pass-the-token
protocol for a shared access path, serialising the team while most messages
requested or released the token; done-conditions demanding durable proof files
made members spend turns proving rather than doing; and jobs aborted on
self-imposed guards before starting. Control services also accept many
concurrent commands, so non-exclusive work need not take turns.

Coordination revision 12 keeps the steering chat responsive: whoever steers a
team assigns goals, resolves consequential choices and unblocks, keeping its
turns short, and delegates implementation, long work and ongoing infrastructure
to members, spawned or assigned. It merges the earlier steering-chat and
infrastructure sentences and shortens others to stay within the 4,000-character
lens limit. In
one run a lead bot spent a whole turn writing and testing a library itself, hit
its 600-second attempt limit, lost the unpublished work and left its team
without steering for ten minutes. The host backs this with advisory notes on
tool results (see [turn continuity](TURN-CONTINUITY.md#deadline-and-steering-notes))
and a `timed_out` advisor fact.

Coordination revision 13 and autonomy revision 8 make verification proportional.
Agents check an outcome once, by the cheapest direct observation, at milestones
or for risky, irreversible or disputed steps; routine steps need no evidence
files, read-backs or proof records, and room posts state the outcome without
evidence paths or measurements. Agents still claim only outcomes they observed.
In run -128, members wrote a "verified" evidence file for every routine step,
read it back and posted a record citing it, spending one or two extra tool calls
per step proving rather than doing. The bot wake text and room post and claim
tool descriptions were softened to match.

Inspect existing task tools and peer findings before implementing common
capabilities again. Share a working reusable implementation with its invocation
and version through existing artifacts or an explicitly shared service.
Reusing code does not require sharing one process or every worker's mutable
state. Existing access controls still apply; this creates no shared-storage or
service-grant mechanism.

Select **Coordination & planning** in the lens picker, or use `/use coordination`.
For a team, select **Swarm** (`/use swarm`), which includes `planning`,
`coordination`, `autonomy` and `investigation`. Swarm revision 2 includes
`planning` directly; a run whose lead posted only a role split and no milestone
backlog showed why the backlog must come first. The first run with coordination alone showed
agents sleeping behind self-imposed safety guards. This is versioned guidance over existing tools, not a central
planner, permission grant or guarantee against deadlock. Its ownership and
blocker advice uses advisory room claims (see
[Room notifications](ROOM-NOTIFICATIONS.md#claims)); claims only help when
agents check them. Existing
chats and running workers are not opted in when the preset is added. Select it
before launching the team; spawned work and continuations keep captured revisions.
Evaluate useful actions, dependency releases, repeated messages and interventions
in a separate opt-in run before claiming an improvement.

## Bounded investigation

The optional `investigation` lens includes Evidence and supplies a procedure over
existing tools. It does not add a scheduler, experiment executor or new tool API.
When an assumption or repeated failed action blocks a plan:

1. Read prior task notes and state the blocker with evidence references.
2. Form a hypothesis and choose the smallest authorized probe, recording the
   expected observation and what would disconfirm it.
3. Run the probe with existing inspect/execute tools; record the result and next
   decision. Repeating an unchanged rejected action is not a new experiment.
4. Use at most three distinct probes for one approach unless the operator
   specifies another allowance. At that checkpoint stop repeating the approach
   and compare established alternatives. A materially different adapter, proxy or
   tool starts a fresh allowance; record why it addresses the failure. Renaming
   the investigation, changing a minor parameter or assigning another worker does
   not reset the same approach's allowance. Overall effort/resource/model budgets
   still apply across approaches. Do not present this default as an operator-imposed
   effort limit.
5. Check known alternatives and peer findings before escalating. Continue an
   authorized, reversible alternative that preserves the requested outcome;
   reaching three probes alone does not require operator permission. Use the
   existing design-question path when no justified authorized alternative remains
   and operator input is needed, or before changing the goal, broadening access,
   exceeding the user's budget or taking a consequential action outside the mandate.
   Otherwise pursue independent useful work or yield on a real dependency.

Keep the record in the task's normal notes and working-state handoff/checkpoint:
blocker, hypothesis, probe, expected/actual evidence and next decision. On restart,
inspect uncertain outcomes before retrying. A quiet healthy download or calculation
does not itself call for investigation or a new deadline. Verified reusable
findings can be proposed for knowledge review; they are not admitted automatically.

Investigation revision 2 scopes the allowance to an approach rather than the
whole blocker. The allowance is prompt guidance, not a host-enforced counter;
it grants no new authority. Existing captured contexts retain their revision.
This change does not establish that a model follows it or improves at every workload;
measure repeated actions, useful outcomes and operator interventions in opt-in trials.

## Team advisor

The team advisor helps the lead steer, like a subagent catching gaps the lead
could miss. It is not a supervisor, planner, relay or churn monitor, and it
holds no authority.

**Strictly opt-in.** Select **Team advisor** on the root chat (tick it, or
`/use advisor`), usually alongside `swarm`. The `swarm` preset does not include
it. The lens offers the `team.advisor` tool; unless the root chat's lenses offer
it, no advisor can be spawned or requested, and Control does not compute the
team's lifecycle topic at all. This is an availability gate, not authority:
selecting the lens only makes the option exist for that task tree. The
operator's bot approval still decides whether an advisor runs, and host checks
still bound what it can do. Unticking the lens stops its facts.

The advisor is an existing persona, not a new role: a read-only **judge**
(see [delegation](DELEGATION.md)). The lens is its procedure and voice; policy
comes from the judge role and the operator's bot approval.

1. The lead starts it with `control_agent_spawn` and `role: "judge"`, one
   objective naming the shared outcome. It gets no resources. It uses the root
   task's advisor allowance (`max_advisors_per_root`, default 1), which is
   separate from the root's one blocking judge review, so a team can have both.
2. The lead asks for it with `control_bot_request` (no interval). The operator
   approves it like any bot, or types `/bot on` in its chat. The lens or a
   request alone runs nothing.
3. The approved judge sleeps on its task tree's lifecycle topic,
   `task:<root chat>`. Control offers host facts there, edge-triggered from records
   reconciliation already reads. Facts are gaps, not churn:
   - the team went quiet, with no member turn running or queued;
   - a member's latest turns failed or were blocked several times in a row
     (`advisor_failure_streak`, default 3; interrupted and cancelled turns
     neither extend nor break a streak), reported once per streak;
   - a watched restart loop was retired after repeated exits;
   - a claim was abandoned by a lost owner;
   - a member's claim became blocked. The fact names the claim it is blocked on
     (`blocked_on`) with that claim's owner and status (`working`, `blocked`,
     `done`, `abandoned`, `released` or `unclaimed`). Members blocked on the same
     claim are summarised in one entry, so several members waiting on one
     producer read as one bottleneck. A blocked state is reported once per claim
     incarnation; updates that stay blocked on the same claim do not repeat it,
     and becoming blocked again after working is a new fact;
   - two or more members report a similar blocker: blocked claims' notes and
     the latest result of members on a failure streak are compared as
     lower-cased word sets (words with digits, such as IDs, ports and times,
     are ignored; Jaccard ≥ 0.6, no embeddings). One entry lists the members
     and one reported reason. A group is reported when a member joins it;
     unchanged reasons are not compared again;
   - the team is busy but stalled: `advisor_stall_turns` (default 24) member
     turns settled while members held working or blocked claims and none
     advanced. A claim advances when it becomes done or resumes working after
     being blocked (the host records `advanced_at` on the claim); acquiring,
     re-reporting or posting is not progress. A stall is reported once; an
     advance, or open claims appearing or clearing, restarts the window. The
     window counts turns, not elapsed time. Teams that do not use claims never
     stall;
   - a bottleneck: one live (working or blocked) claim that the claims of
     `advisor_bottleneck_members` (default 2) or more members other than its
     owner wait on, directly or transitively through `blocked_on`. The fact
     names the claim, its owner and status, what it is itself blocked on, a
     circular wait if its dependents lead back to it, its age in claim updates
     since it was acquired and since it last advanced, and the waiting members
     and claims. It is derived from claim state alone and reported once per
     claim incarnation and dependent set; it is reported again when that set
     changes. The dependency graph is walked only when a member claim changes;
   - a member cannot checkpoint: more than `bot_recovery_continues` (default 2)
     of its turns ended without an accepted handoff since its last published
     one. The fact names the member, the count and the host's last handoff
     rejection, if any; it is reported once per streak;
   - a turn timed out: a member's latest turn, or the lead's, reached its
     attempt time limit and was stopped before it checkpointed (`timed_out`,
     naming the lead as the lead). It is reported once until that chat's
     next turn settles otherwise. The advisor suggests splitting the work, or
     for the lead, delegating it to a member.

   Apart from a timeout, a single failed, interrupted or cancelled turn
   (provider errors included) and a single failed watch exit are never facts. Facts coalesce into one
   reason under the bot's existing 30-second cooldown and 60 turns per hour.
   There is no clock rule: a quiet team is reported once per settled change,
   and elapsed time is never a fact.
4. On waking, the advisor reads the facts and `control_room_read` (participants,
   posts and claims with `owner_idle`/`abandoned` flags). If there is a gap, it
   sends the lead's steering chat one concise note with `control_agent_message`:
   the evidence and a suggested steering step. For blocked members it checks
   whether the input is one they could quickly make themselves, or several wait
   on one producer, and suggests an unblocking step: produce the input
   themselves, reassign the blocking claim, or an alternative. For a bottleneck
   it suggests putting more members on it, splitting it, or re-routing
   dependents around it. It sends nothing
   when evidence is unchanged.

The advisor only steers. A note goes only to the lead's steering chat, as
retained peer evidence with source `advisor-note`, shown as "Advisor note from …"
in its own box. An active lead gets one callback hint at its next tool
response; a lead running as an approved bot wakes within its own budget;
otherwise the lead sees it at its next turn. The lead decides what, if
anything, reaches the team, for example by revising the backlog or messaging a
worker. The advisor never messages members and never posts to rooms.

The host refuses notes from judges that are not approved advisors, notes to any
chat but the advised team's steering chat (members, judges, other advisors),
notes that share services, and a note identical to one already delivered. A
note to a lead whose latest turn failed or was cancelled is refused by the
existing peer rule that messages cannot restart stopped work. The exception is
a lead that is itself an approved persistent bot, not stopped, whose turn
failed or was interrupted: the note then waits for its next supervised wake.

Loop safety is structural. Judge turns, retained notes (runs without an
attempt) and turns woken only by advisor notes, including the lead's, are not
team activity, so the advisor cannot re-trigger itself through the team it
advises. Stopping the advisor (`/bot off`) drops its team snapshot; a later
advisor reports only facts after its own subscription. Nodes without an
opted-in advisor do no lifecycle work.

## Ganglia

**Ganglia** (`/use ganglia`): reflexes and routines run locally; turns are
strategy. An octopus's arms act through local ganglia while its brain decides
where to go. Use it for long-running work on systems that change faster than a
turn, not one-off tasks or ordinary coding, which the autonomy routine rule
covers. Examples: a game agent or a robot (reflexes keep it safe, skills move or
build), a long scrape (reflexes back off on rate limits and rotate on errors,
skills crawl a section), price monitoring (reflexes retry and alert on outages,
skills watch a basket and report thresholds), or a live service (reflexes
restart and shed load, skills run maintenance).

The lens text itself is domain-independent; examples like these live only in
documentation. Revision 2 changed its stance from "build one shared reflex
layer first" to **reuse first**: when an admitted or available
[tool package](TOOL-PACKAGES.md) already provides reflexes or skills, use it and
read its README and notification contract; build or extend a layer only for a
gap a run has demonstrated; keep every layer minimal. Reflexes commit, skills
are bounded routines toward a target (including timed sequences the model
parameterises) that escalate only what they cannot handle, and turns set goals
and re-plan. New or extended layers live in one folder ready to save as a tool
package. It turns the operator's repeated steering in runs -121 and -128, which
drove a system one tool call per action, into standing procedure, and adds no
tool or authority. It is guidance on demand: prompts carry its summary line.

Revisions: r1 (build a shared reflex layer early, then skills; game-flavoured
wording such as threats and manoeuvres), r2 (domain-independent wording, reuse
an existing package first, build only for a demonstrated gap, on-demand
summary).

**Pitfall: jitter.** In run -128 the shared reflex layer thrashed: a player
surrounded by hostiles walked a few metres, stopped, swung once and re-planned.
Every 500 ms tick re-decided on the nearest threat alone, any threat in a wide
radius cancelled the routine (forcing a fresh path search), each reaction acted
once, and the mode flipped at a health threshold. Reflexes must commit: hold
until the trigger stays clear, act continuously, weigh every threat, preempt
only for imminent ones, and pause rather than cancel routines.

**Pitfall: patching layers.** In the same run a member spent a 45-minute turn
repeatedly patching movement code entangled with reflex leases instead of
placing blocks. Keep each layer minimal; a layer that blocks the task is
bypassed or removed, not patched again and again.

Opt-in: `swarm` does not include it, so the scorecard can compare runs with and
without it.

## Tool packages

A lens can also name operator-admitted [tool packages](TOOL-PACKAGES.md), such as
`mc263-client@1` (pinned) or `mc263-client` (the latest admitted version when a
message is sent). Runs capture the exact version and commit with the rest of the
context. Each attempt then gets a private copy at `/workspace/.control-tools/NAME`
and can read the manifest with `control_package_read`. Runs without such a lens
see no packages, so a run that should rebuild a tool from scratch leaves it out.

## Current boundaries

Tool hints contribute to context and catalogue discovery. They do not currently
filter every callable worker tool, and deselecting a hint does not revoke a grant.
The one exception is `team.advisor`, which decides whether the opt-in advisor
option exists for a task tree. It is an availability gate, not a grant.
Host operations enforce actual execution authority independently. Captured
documents are attributed evidence/context, not instructions or automatically
accepted facts. Inspect the captured context when checking what a worker received.

**Tool exposure by role (scoped, not implemented).** Prompts are now role
scoped; tool lists are not. Every managed worker session still receives the
full worker tool list: 47 core schemas, about 46.6 KB of JSON, before room
tools, which is more than the whole on-demand prompt. The run -128 review found
members used about 15 of 66 `control_*` tools. Native CLIs only call tools
listed at session start, so hiding a tool means a call-through: a small default
set per role (turn context and handoff, workspace and service commands, room
read/post/claim, agent message, watches, guidance, recovery) plus a catalogue
search returning schemas and one dispatcher that calls a listed-elsewhere tool
by name, under the same host checks. That changes the broker, both adapters and
their tests, so it is a separate change; the wake-size fixture in
[PERFORMANCE.md](PERFORMANCE.md) reports the schema bytes to measure it against.

New built-ins are added to older libraries at startup when library capacity
permits, without replacing a local definition of the same ID or selecting a lens
for existing chats. No experiment runs merely because a preset becomes available.
