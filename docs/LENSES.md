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

## Task autonomy

The optional `autonomy` lens asks the agent to own its outcome, balance progress
against task-specific risk, and prefer reversible steps with evidence and a
recovery path. A task-local guard can embody an incorrect assumption; investigate
its rationale rather than treating every guard rejection as a reason to wait
forever. Host-enforced permissions, mandatory safety constraints, cancellation
and explicit user limits remain binding. The lens does not authorize broader
access or consequential actions beyond the task's mandate. Long-lived workloads
need a current job and a path to the next authorized job, rather than only a
connection or health monitor. A finished job means notify and take the next one,
not exit; a setback means recover, not quit. Test their notification path before waiting, then
notify on completion, actionable blockers or transitions that open useful work.
Include the job ID, outcome and evidence reference; deduplicate repeated outcomes
and keep routine diagnostics out of notifications. Use the workload's existing
interface to assign work without restarting a healthy persistent process for
every action.

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

Inspect existing task tools and peer findings before implementing common
capabilities again. Share a verified reusable implementation with its invocation,
version and evidence through existing artifacts or an explicitly shared service.
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
     rejection, if any; it is reported once per streak.

   A single failed, interrupted or cancelled turn (provider errors included)
   and a single failed watch exit are never facts. Facts coalesce into one
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
strategy. An octopus's arms act on their own through local ganglia while its
central brain decides where to go. The lens is the build procedure for that
split when a team drives an external system repeatedly.

Use it for long-running work with reflexes: systems that act faster than a turn,
such as games, robots, live services or monitoring. It is not for one-off tasks
such as scraping a website or ordinary coding; the task autonomy routine rule
already covers those. The procedure:

1. Build layers bottom-up. First one shared always-on reflex layer for
   reactions faster than a turn (protect, recover, maintain), built early by one
   member while others keep working and reused by every member instead of each
   embedding its own guards. Then skills: bounded routines on top of it that work
   toward a target. Supervise from their events.
2. Routines observe and filter, escalating only salient changes (milestones,
   failures, threats they cannot handle), never raw streams.
3. Composite manoeuvres (sequenced, timed behaviours) are skills too: the model
   selects and parameterises them rather than issuing their steps.
4. Turns are strategy: choose goals, assign skills and change plans from
   escalations, without re-checking the world each step.
5. Reflexes and skills live in one self-contained folder with a README and
   notification contract, ready to save as a [tool package](TOOL-PACKAGES.md).

Runs -121 and -128 drove an external system one small job per tool call, and
the operator repeatedly steered them towards exactly this: a shared safety and
upkeep loop, skills that run to a target, and escalation only of what matters.
The lens turns that steering into standing procedure. It applies the task
autonomy rule that a routine reacts and the model decides; it does not restate
it, and adds no tool, scheduler or authority.

Ganglia is opt-in: the `swarm` preset does not include it, so the scorecard can
compare otherwise identical runs with and without it before any claim that it
helps. Select it alongside `swarm` before launching a team.

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

New built-ins are added to older libraries at startup when library capacity
permits, without replacing a local definition of the same ID or selecting a lens
for existing chats. No experiment runs merely because a preset becomes available.
