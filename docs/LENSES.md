# Lenses and bounded investigation

A lens is versioned context: instructions, captured knowledge documents,
descriptive tool hints and other lenses. It is not an agent identity or a grant.
Use **Lenses** to create/edit definitions and inspect their effective instructions.
Select them in a chat's lens picker, or use terminal commands:

```text
/use engineering autonomy investigation
/context
```

The same selection is available through `control.chat.lenses`. Changes affect
future operator messages; queued runs, spawned workers and automatic continuations
retain their captured revisions. Editing the library never changes an active
attempt's instructions. `/use none` clears the selection for future messages.
The browser preference only chooses the starting selection for a new chat.

## Task autonomy

The optional `autonomy` lens asks the agent to own its outcome, balance progress
against task-specific risk, and prefer reversible steps with evidence and a
recovery path. A task-local guard can embody an incorrect assumption; investigate
its rationale rather than treating every guard rejection as a reason to wait
forever. Host-enforced permissions, mandatory safety constraints, cancellation
and explicit user limits remain binding. The lens does not authorize broader
access or consequential actions beyond the task's mandate.

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
