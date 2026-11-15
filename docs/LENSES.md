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
connection or health monitor. Test their notification path before waiting, then
notify on completion, actionable blockers or transitions that open useful work.
Include the job ID, outcome and evidence reference; deduplicate repeated outcomes
and keep routine diagnostics out of notifications. Use the workload's existing
interface to assign work without restarting a healthy persistent process for
every action.

Weigh the actual recovery cost of a setback against the cost of stalled work;
recoverable does not mean harmless, especially for shared assets. Routine
independent work within the mandate needs no peer permission. Uncertainty alone
calls for investigation with existing observations and tools, rather than stopping.

A quiet healthy download, calculation, service or genuine dependency wait needs
no periodic model wake or invented deadline. Workload-specific idle diagnostics
are optional and must be actionable, rather than repeated no-change polling.

## Reusable planning procedure

Select **Planning procedure** (`/use planning`) for a broad outcome, or select
**Swarm** (`/use swarm`) to combine it with coordination, autonomy and investigation.
The existing lens library is the versioned procedure mechanism: there is no new
planner API, task queue, scheduler or provider-specific skill loader. The same
procedure is available in the web picker, TUI and `control.chat.lenses`.

Keep a short rolling plan in the task's normal notes or shared room. Reference it
instead of mirroring the whole backlog in every message. For each active
deliverable record its outcome, completion evidence, owner, actual prerequisites
and next action or release signal. Start with a useful end-to-end result and
reliable reusable capabilities, then expand. For example, a team building an
integration might first deliver one working request, then error handling and
recovery, then additional operations. Connection health alone is not the feature.

Use existing room claims for overlapping shared work, not every local action.
Workers choose ready work, update their claims and take the next authorized step
without a steering relay. A workload may chain its remaining authorized jobs
itself, or notify its owner to assign the next job. Before yielding, retain the
current job, remaining work and the callback or claim release that enables the
next action in the existing handoff. Simple local tasks need no team plan.

## Coordination and planning

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

Roles and claims own shared deliverables, not exclusive rights to routine inputs
or another agent's basic task prerequisites. Check that a dependency's producer
can act and is not waiting on you. If it cannot act, coordinate a handoff of
ownership or do independent authorized work; elapsed time alone does not justify
duplicating healthy work. When nobody can unblock the goal, ask the operator
with options and apply standing guidance to similar authorized steps. Assign
ongoing shared services to workers that peers can contact directly, rather than
making the steering chat an operational dependency.

Inspect existing task tools and peer findings before implementing common
capabilities again. Share a verified reusable implementation with its invocation,
version and evidence through existing artifacts or an explicitly shared service.
Reusing code does not require sharing one process or every worker's mutable
state. Existing access controls still apply; this creates no shared-storage or
service-grant mechanism.

Select **Coordination & planning** in the lens picker, or use `/use coordination`.
For a team, select **Swarm** (`/use swarm`), which includes `coordination`,
`planning`, `autonomy` and `investigation`: the first run with coordination alone showed
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
4. Use at most three distinct probes for the same question unless the operator
   specifies another allowance, within the existing task/resource/model budgets.
   Do not reset that allowance by renaming the investigation.
5. Change approach, seek targeted peer help, or record an explicit blocker and
   yield on a real dependency. Consequential choices outside the mandate use the
   existing design-question path.

Keep the record in the task's normal notes and working-state handoff/checkpoint:
blocker, hypothesis, probe, expected/actual evidence and next decision. On restart,
inspect uncertain outcomes before retrying. A quiet healthy download or calculation
does not itself call for investigation or a new deadline. Verified reusable
findings can be proposed for knowledge review; they are not admitted automatically.

The three-probe allowance is prompt guidance, not a host-enforced counter. This
change does not establish that a model follows it or improves at every workload;
measure repeated actions, useful outcomes and intervention in opt-in trials.

## Current boundaries

Tool hints contribute to context and catalogue discovery. They do not currently
filter every callable worker tool, and deselecting a hint does not revoke a grant.
Host operations enforce actual execution authority independently. Captured
documents are attributed evidence/context, not instructions or automatically
accepted facts. Inspect the captured context when checking what a worker received.

New built-ins are added to older libraries at startup when library capacity
permits, without replacing a local definition of the same ID or selecting a lens
for existing chats. No experiment runs merely because a preset becomes available.
