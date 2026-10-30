# Delegated agents and judges

Workers can call `control_agent_delegate` with an `objective` and `role`:
`worker` or `judge`. Control checkpoints the current files, releases the worker,
runs the child, and resumes the parent with bounded feedback. The web transcript
links to the child conversation; Agents shows its execution and queue status.
Execution is serial for now. A model must stop after delegation acknowledgement,
rather than poll or hold a command open waiting for a child.

Each child has the same actor, captured lenses and authority ceiling. Its files
come from an immutable copy of the parent's paused checkpoint, including in
Improve mode. Children get independent writable workspaces; edits never merge
automatically. Feedback includes a bounded diff excerpt and a link to retained
results. The parent can use the feedback to make its own changes.

Use `control_agent_result` with the delegated child ID and `section` set to
`summary`, `files`, or `file` to retrieve larger outputs. Follow `next_offset`
until it is null; text offsets count Unicode characters. File pages include
hashes and change/deletion metadata when a scoped baseline is available. Reads
follow the child's recorded continuations and verify published artifact hashes.
Only the requesting parent's actual delegated descendants are available; sharing
a root or actor does not grant access to unrelated chats or sibling agents.
The tool never reads live workspaces. If no viewer artifact was published, it
reports that explicitly; existing artifact size and file-type limits still apply.

Personas currently capture a role and enforced policy, not persistent autonomous
bots. A worker may delegate to depth two; a root task permits eight children in
total and one independent judge review. A judge uses the first configured model
from a different provider. Configure both providers in Models & providers first;
missing configuration fails explicitly rather than reviewing with the same model.

Judges have read-only project mounts and no delegation, project-access requests,
preview publishing, or administration tools. They can inspect source, use isolated
verification tools and collaborate through the linked room. Claude workspace
commands inspect `/workspace`; Codex inspects `/review` and uses `/workspace`
only for scratch and transport. `/tmp` is available for disposable verification
outputs. Their reports do not advance the development workspace.

Waiting state and child/continuation IDs are persisted together. Restart does not
create duplicate children; interrupted or failed children return that status to
the parent. Cancelling a supervisor cancels its pending descendants. A changed
parent workspace fences continuation. Merging, deployment and additional host
access remain explicit operator actions.

These are bounded node tasks using the configured providers. They are not an
unattended evaluation or judge swarm. Use provider accounts and API keys within
the applicable terms and usage limits.
