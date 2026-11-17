# Workspace commands

Agents run commands in their task workspace with these tools:

| Tool | Purpose |
| --- | --- |
| `control_workspace_run` | Short command; 25-second deadline, bounded output. Claude and local-model sessions only. |
| `control_workspace_start` | Background job; returns a `job_id` immediately. |
| `control_workspace_status` | Status, bounded output and exit code. Does not wait. |
| `control_workspace_cancel` | Stop a job. It reports `stopping` until its container has exited. |

`control_workspace_start` takes a `command` (1–16,000 bytes), an optional
`timeout_seconds` (1–1800, default 600) and an `environment`: `worker` (default,
the attempt's captured worker image), `build` (the
[declared build environment](BUILD-ENVIRONMENTS.md)) or `solver` (the pinned
[solver image](SOLVERS.md)). Every image must be pinned by digest. The job
deadline never extends past the owning attempt's deadline; use a
[persistent service](PERSISTENT-TOOLS.md) for work that must outlive the attempt.

## Limits

- One active job per attempt, at most 32 jobs per attempt.
- Replaying the same request nonce returns the existing job instead of running
  the command again.
- Output streams into the chat and TUI activity. Programs and the container log
  driver may buffer; newline-terminated output appears while the command runs.
  The retained tail is 16,000 bytes plus an omission marker. Container logs
  rotate within two 1 MiB files.
- `worker` and `solver` jobs: 2 GiB memory, two CPUs, 256 processes.
  `build` jobs: 4 GiB memory, two CPUs, 512 processes. All set
  `CARGO_BUILD_JOBS=2`.
- `/tmp` is a 1 GiB memory-backed scratch; `/home/agent` is 128 MiB.

## Isolation

Commands share the attempt's isolated workspace at `/workspace`. They get no host
directory, credentials, daemon socket or network, and run with all capabilities
dropped and `no-new-privileges`. The image is never modified.

Read-only (judge) attempts mount the project read-only and get a private writable
`/build-target` (`CARGO_TARGET_DIR`) for verification builds. It lives in the
attempt's state directory, outside project checkpoints and artifacts.

## Lifecycle

Jobs end with their owning attempt. Before publishing a checkpoint or pausing for
a question or delegation, Control cancels outstanding jobs and confirms their
exact containers have exited. If a stop cannot be confirmed, checkpoint
publication stays fenced. Agents should wait for builds to finish before ending
their turn. Cancelling does not roll back edits already made in the workspace.

After a restart, interrupted jobs are not rerun. Reconciliation matches each
persisted job's node, name, image, workspace and container ID before stopping it;
a replaced or uninspectable container keeps dispatch fenced for inspection.
Progress is saved every 15 seconds and on lifecycle transitions, so a crash can
lose the latest output.
