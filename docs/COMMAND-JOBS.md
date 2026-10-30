# Workspace commands

Agents can use `control_workspace_start` with a command, an optional deadline
(1–1800 seconds; default 600), and a declared `worker` or `build` environment.
The tool returns a `job_id` immediately. `control_workspace_status` reads its
status, bounded output and exit code; `control_workspace_cancel` requests its
termination. Claude's short `control_workspace_run` tool remains available.

Command output appears in the existing chat and TUI activity stream. Programs
and the container log driver can buffer output; newline-delimited output is
visible while a command runs. The retained tail is at most 16 KiB, plus an
omission marker. Container logs rotate within two 1 MiB files and are removed
when the verified command resource is stopped. Final bounded diagnostics remain
in the attempt record.

Each attempt admits one asynchronous command at a time, with at most 32 jobs.
Replaying the same request nonce returns the existing job rather than running
the command twice. Commands share the attempt's isolated workspace; no host
directory, credential, daemon socket or external network is provided. The
selected image is pinned and read-only. Scratch space is private to the job.
Judges receive a read-only project mount and a separate writable disk target
directory for verification builds. Their target files stay in the attempt's
private state directory and are excluded from project checkpoints and artifacts.
Build jobs have 4 GiB memory, two CPUs and two Cargo build workers; ordinary
jobs have 2 GiB and two CPUs. Temporary memory-backed scratch is limited to 1 GiB.

Jobs end with their owning attempt. Before publishing a checkpoint or pausing
for a question or delegation, Control cancels outstanding jobs and confirms
their exact containers have exited. A failed stop fences checkpoint publication.
Agents should await successful builds before finishing their turn. Cancellation
does not roll back edits already made in the private workspace.

After a control-plane restart, interrupted jobs are not rerun. Reconciliation
matches each persisted job's node, name, image, workspace and captured immutable
container ID before stopping it. A replacement or uninspectable resource keeps
dispatch fenced for inspection. Progress is saved periodically and on lifecycle
transitions; an abrupt crash can lose the latest unsaved output.
