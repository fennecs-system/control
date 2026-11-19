# Persistent tools

A service is an offline container owned by one chat and actor on one node.
Model attempts can attach to it across turns; finishing or cancelling a turn
does not stop it. Explicit stop, permission revocation, command exit or lease
expiry starts cleanup. A service is not a model session and does not use a
model-turn slot.

## Sharing services

Delegated workers inherit captured grants for exact service generations. This
does not change ownership or expose other chats' services. Design continuations
keep their grants; completed delegated work returns its resource handles to the
parent. Judges receive no service grants.

To share a service created after spawning, attach it to a
`control_agent_message`:

```json
{"chat_id":"<peer chat>","text":"The service is ready; continue using it.","services":["<service ID>"]}
```

- The sender must already have access. The recipient must be a same-actor
  descendant or another worker under the same top-level spawned task; the
  steering chat is not a peer destination.
- The message and its exact owner/generation grants commit together. Grants
  activate in the recipient's next authorized attempt, not an active one. A
  replacement service generation needs a new grant.
- A grant permits the existing service operations, including exec, copy and
  stop. It is not a port permission: [scoped TCP ports](PORTS.md) remain a
  separate operator decision. Omitting `services` transfers no authority.
- Retries are idempotent; a failed checkpoint rolls back the message and grants.

Delivery, wake and urgency rules for peer messages are in
[room notifications](ROOM-NOTIFICATIONS.md). Peer messages are coordination
evidence, not operator steering: they cannot bypass a bot's pause/stop, revive
cancelled work, change action scope or continue a judge.

## Task coordination threads

Each new task coordinates in a thread under `general`, `engineering` or
`research`. The thread is keyed by the top-level chat, so workers spawned before
or after the lead attaches resolve the same thread, and continuations keep it
across restart. A worker using `room: "general"` reads and posts within its own
task's thread; other tasks' history, links and callbacks are excluded. Threads
reuse the existing room records, inbox cursors and subscriptions; they add no
supervisor state, polling or deadline.

The Rooms page lists task threads by their lead's title. Operators can read and
post in a thread or start **New chat in this thread**. Exact addresses are
`<channel>/<thread chat ID>`; worker tools cannot choose another task's thread.
Legacy shared-room attachments stay shared so an upgrade does not move an active
swarm. To move an old chat, disable its bot/listener, then use `/room general`
(or another channel) to attach its task thread. New threads do not import prior
shared history.

## Tools

Ordinary Codex and Claude worker attempts get these tools; the Tools page uses
the same catalogue. Judges cannot call them, and the operator stdio/HTTP MCP does
not expose service management.

| Tool | Result |
| --- | --- |
| `control_service_start` | Checkpoint a command and return its durable handle; startup is asynchronous. |
| `control_service_status` | Inspect one handle, or list active services and ten recent completed handles. Each shows its state, limits, ports, watches and the caller's latest executions. `detail: "node"` adds shapes, the remaining node budget, the volume name and other callers' executions. |
| `control_service_attach` | Read the same status and bounded logs for an owned handle; does not extend its lease. |
| `control_service_stop` | Record stop intent. Wait for `Stopped` before assuming removal succeeded. |
| `control_service_exec` | Run a bounded command in an owned running service. Waits up to `wait_seconds` for the result, else returns an `execution_id`. |
| `control_service_exec_status` | Read a command's live output, durable status and exit code; `wait_seconds` long-polls. |
| `control_service_copy` | Copy a file or tree from the task workspace into the service's `/workspace`; waits like exec. |

Services run in a fresh container from the attempt's captured, immutable Nix
worker image, with the trusted `control-service-runner` as entrypoint. Images
built before that runner existed are refused; rebuild them with the Nix setup.
No image is modified or committed from an agent's writable state.

## Isolation and storage

- No Docker network, provider credentials, Docker socket or host/attempt mounts.
  [Scoped TCP ports](PORTS.md) use operator approval and trusted relays; they do
  not enable the container's network.
- Non-root user, dropped capabilities, no new privileges, private PID namespace,
  no automatic container restart.
- `/workspace` is a Docker volume owned by the node and chat. Replacement
  services in the same chat reopen it, including after process exit, container
  cleanup or Docker restart. Same-chat services share it and must avoid
  conflicting writes; other chats and nodes get different volumes. Volume names
  are derived by Control, never taken from tool input. There is no Control disk
  cap; the engine's disk applies. Attempt checkpoints do not include these
  files, and a volume is not a backup against disk loss. `status.workspace`
  is set for a retained volume (its name with `detail: "node"`); absent means an older service with the legacy in-container
  layout, which is never migrated silently.
- `/home/agent` and other writable-layer files are lost when the container is
  removed. `/tmp` is a 128 MiB tmpfs counted against the memory limit; use a
  scratch folder in `/workspace` for larger temporary data.
- Optional `storage_limit_mb` asks Docker to cap the private writable layer
  (not `/workspace` or other mounts). It needs a quota-capable storage driver
  such as overlay2 on XFS with project quotas. Unsupported caps fail creation
  with a diagnostic; Control never drops them silently.

Keep task programs, worktrees, downloads and durable data under `/workspace`.
Commands start there. This is a storage contract, not a write jail: files
written elsewhere are simply not retained. Export results as artifacts or
project checkpoints when they must outlive the service's storage.

Container removal does not remove workspace volumes. To inspect them:

```sh
docker volume ls --filter label=dev.control.purpose=service-workspace
```

and check their `dev.control.node` and `dev.control.chat` labels. Delete a volume
only after confirming its data is no longer needed; `docker volume prune` can
destroy retained workspaces. The Services page can delete stopped services one at
a time or with **Clean up stopped services**. Once no service record of a chat
remains, that also removes the chat's volume; only volumes carrying this node's
Control labels are removed, and the engine refuses one still mounted.

Control does not restart application processes or replay commands after a
Docker restart. Start a replacement service and inspect its workspace before
relaunching the application.

## Compute

Omit `shape` to use the image default: 1 CPU, 512 MiB RAM and 64 processes
(Full image: 2 GiB and 128 processes). Presets are independent of the Lite/Full
image:

| `shape` | RAM limit | CPU limit |
| --- | --- | --- |
| `small` | 2 GiB | 1 |
| `medium` | 4 GiB | 2 |
| `large` | 8 GiB | 2 |

Preset limits are immutable and saved with the service. Resizing means stopping
and replacing the service in the same chat; it reopens `/workspace`, but ports
and grants must bind the new handle. Leave headroom inside the limit: a managed
runtime's heap must be smaller than the container's memory limit.

The process (pids cgroup) limit counts threads. It is `service-pids-per-gib`
(default 64, 1–4096) per GiB of RAM, capped at `service-pids-max` (default 1024,
128–65536) and never below the image default. With the defaults: small 128,
medium 256, large 512. The value is captured at admission and used both to create
and to verify the container, so changing the settings affects only new services.
Status and the Services page report `pids_limit`. At the limit, new commands fail
with `Resource temporarily unavailable`.

The node service budget admits services atomically with their durable
reservation:

| Node setting | Default | Range | Meaning |
| --- | --- | --- | --- |
| `service-memory-gib` | 20 | 1–4096 | Strict sum of service memory limits. |
| `service-cpus` | 8 | 1–1024 | CPUs available to services. |
| `service-cpu-oversubscription` | 2 | 1–8 | CPU limits are caps, so CPU is admitted against `cpus x factor` (16 by default). |

Settings are files of the same name in the node configuration directory; invalid
or absent values keep the default. With the defaults, 16 Lite services fit at
image-default limits. A separate ceiling of 128 live service records bounds state
and engine inventory. Pending, stopping and fenced services keep their reservation
until teardown is confirmed. An over-capacity request fails without creating
anything, and retries do not reserve twice.

This is an admission budget, not a host allocator. External model servers,
worker containers and Control itself need extra headroom, and the limits do not
guarantee resident memory or dedicated cores. Unsupported engine limits fail
visibly, never uncapped.

## Leases and logs

- `lease_seconds: -1` (default) keeps a service until stopped; 1–3600 sets a
  bounded lifetime. Port approvals default to the service's lifetime and never
  outlive it.
- Use a foreground command: its exit ends the service and its descendants.
  `exec sleep infinity` keeps an idle service alive while you set it up with
  `control_service_exec`. Background programs must redirect stdin/stdout/stderr
  to private files.
- Docker logs rotate at two 1 MiB files. Attach and status return the last 80
  lines. Final bounded output and the exit code are saved before removal, so a
  short failed probe stays inspectable. The node keeps at most 128 service
  records, evicting the oldest stopped one whose originating attempt is no
  longer running.

## Commands

`control_service_exec` takes `timeout_seconds` (1–1800, default 120), capped by
the lease. Admission wakes the service supervisor, which launches at once rather
than on its 2 s reconciliation tick. Exec and copy then wait inline up to
`wait_seconds` (0–30, default 15) and return the final result when it finishes in
time; otherwise they return an `execution_id` to poll with
`control_service_exec_status` (`wait_seconds` default 0). A wait holds no state
lock and ends early when the attempt is cancelled or replaced, Control stops, or
3 s before the attempt's deadline. The 30 s maximum stays below the worker
bridge's 35 s reply timeout. `wait_seconds` is not part of the request identity:
a retry with the same nonce waits on the same execution and never resubmits.

Commands from every agent sharing a service run concurrently, each with its own
`execution_id`, timeout and output. The service's own resources are the real
bound: each command is at least one process, so a service runs at most
`pids_limit` commands. `service-commands-max` (default 64, 1–1024) is a safety
ceiling on Control's side (one supervisor thread and engine client per running
command). At the limit, a new command is refused, not queued, with the active
`execution_id`s and their owners.

Each persisted command record keeps a 2,000-character label and up to 16,000
bytes of output (head and tail around an omission marker); the command text is
dropped at launch. Up to 16 finished outputs are kept beside active commands.
Evicted output leaves a retry receipt (at most 1,024 per service) while its
attempt can still call tools; status then reports `output_evicted=true`. Retries
return the recorded outcome rather than re-executing.

A timed-out or interrupted command is stopped inside the service with every
process it started, detached ones included; the service keeps running. Only if
that cannot be verified is the service stopped, because killing the host Docker
client alone would not prove the remote writer stopped. A restart never replays
an in-flight command.

Commands are for one-off work: setup, inspection, and installing or editing a
routine. Repeated or polled work belongs in a [routine](WATCHES.md#routines), and
long-lived clients, servers and daemons belong in a watch (`control_watch_start`,
see [WATCHES.md](WATCHES.md)): `restart=transient` runs finite work such as large
downloads once; `restart=permanent` restarts after any exit. Neither has a task
deadline.

These tools provide no host command execution, arbitrary container selection or
new network authority. Task-specific adapters and server configuration stay
outside Control's source.

## Copying task files into a service

`control_service_copy {"service_id", "from", "to"}` brings files, such as a
project folder opened with `/open`, into a service without a host mount or exec
heredocs. `from` is relative to the attempt's task workspace (approved project
folders included); `to` is relative to the service's `/workspace`. A leading
`/workspace/` is accepted on either. Every segment must be a plain name: no
absolute paths, `..`, `.` or empty segments. No host paths are accepted or
returned.

- **Authority.** Only the caller's active, non-judge attempt can copy, only from
  its own workspace, and only into a service it can already exec in.
- **Source safety.** Reads walk pinned directory descriptors with no-follow
  opens, using the artifact publication policy. Symlinks, FIFOs, sockets,
  devices, non-UTF-8 names and excluded names (`.git`, `.env*`, `auth.json`,
  `.claude`, `.codex`, `.control-*`, `target`, `node_modules` and similar) are
  skipped without being followed and reported with up to eight examples. Depth
  is limited to 32.
- **Tool packages.** Admitted [tool packages](TOOL-PACKAGES.md) at
  `/workspace/.control-tools/<name>` are the only Control-internal source a copy
  may read. No destination may use a `.control-*` name.
- **Bounds.** The tree is checked against the byte and entry bounds before
  admission, before launch and while streaming, so an oversized source writes
  nothing. Defaults are 256 MiB and 20,000 files plus directories; set
  `service-copy-max-mib` (1–8191) and `service-copy-max-entries` (1–1,000,000)
  in the node configuration directory. A memory-backed or capped legacy
  `/workspace` lowers the byte bound to its size.
- **Streaming.** Control writes a ustar/pax archive (directories and regular
  files, executable bits kept) through a 64 KiB buffer into `tar -x`, run as the
  service user in `/workspace`. The copy can write nothing that user could not
  already write. Existing files at the same paths are replaced; others are kept.
  A file that shrinks while streaming fails the copy.
- **Exclusive.** A copy takes the service exclusively: it waits for running
  commands, then runs alone, and new commands and copies are refused with its
  `execution_id` until it ends. It reuses the command machinery: durable intent,
  request-identity receipts (a reused nonce with different arguments is refused),
  `timeout_seconds` (default 300), in-service reaping and no replay on restart.
- **Failure.** The source is read only while the requesting attempt is active,
  so finish polling before ending the turn. Extraction failures mark the copy
  failed without stopping the service. Copies are not atomic.

There is deliberately no copy from a service back to the task workspace. That
would extract untrusted output into a host directory, a new host-write boundary
needing its own extractor, checkpoint interaction and storage accounting. Read
small results through bounded command output instead.

## Lifecycle and recovery

`control-core::resources` holds validated IDs, ownership, generation, lease,
scope and a pure transition function. Time, observations and authority are
inputs; it returns a phase and an explicit effect. Docker and storage stay in
application adapters. The crate needs only `std`; persistence uses its optional
`serde` feature with validated deserialization.

The application persists create intent before creating a container and records
its exact ID before starting a command. A lost creation receipt can adopt only a
dormant container with matching identity and scope. A missing Running receipt
reconciles the already-bound ID without starting it twice. An exited or missing
command is never replayed.

Before adopting, starting, removing or reading output, the supervisor checks the
image, node/chat/resource/generation, container name and ID, entrypoint
arguments and enforced scope. Labels alone grant no authority. A mismatch fences
the record for operator inspection; Control never deletes an unfamiliar
container.

Stop intent precedes removal, and only an observed absence completes cleanup.
An explicit stop also retries cleanup of a fenced service, keeping the recorded
identity and reservation until the engine confirms absence (checked by exact ID,
not only label-filtered inventory). An owned paused container can be removed
after verification. Fenced services never restart or resume their command.
Revocation permits cleanup but blocks new create/start effects. This local
protocol does not guarantee exactly-once external effects.

A trusted Rust PID 1 (`control-service-runner`) enforces the recorded lifetime,
even while Control is stopped. It makes itself non-dumpable before spawning the
command and reaps descendants. Linux ends the PID namespace when its init exits,
and same-namespace children cannot send it an unhandled SIGSTOP/SIGKILL (see
[PID namespaces](https://man7.org/linux/man-pages/man7/pid_namespaces.7.html)
and [PR_SET_DUMPABLE](https://man7.org/linux/man-pages/man2/PR_SET_DUMPABLE.2const.html)).
This relies on the Linux container boundary and a trusted engine/host; it is not
VM isolation. Keep host and engine clocks synchronized; a separate maximum
duration bounds the watchdog if they differ.

Idle supervision performs no engine queries or state writes. Active services
share one batched inventory per pass, and unchanged Running observations are
not saved.

## Worker bridge

The worker MCP mailbox accepts single JSON-RPC requests and batches. Batch
elements run in order through the same attempt authorization, with separate
delivery identities; notifications get no reply. Replies are correlated by
nonce. The host claims a request before executing it, so a late reply cannot
delete the next one, and a missing reply never replays an effect. Transport
errors distinguish "not sent" from an uncertain outcome to inspect before
retrying.

The bounded `/workspace/.control-room-mcp.log` (and its `.last` file) records
exchange steps and tool names, without arguments or results, to tell a provider
that has not sent a call from a host that has not answered. These files are
excluded from published workspaces.

## Live checks

Unit fault tests cover checkpoint failures, replayed requests, retention,
ownership/scope mismatches, revocation, expired leases, fenced-record capacity
recovery and containers hidden from label-filtered inventory. The ignored Docker
checks below need a rebuilt worker image and make no model calls. Use scratch
`CONTROL_STATE_DIR` and `CONTROL_CONFIG_DIR` (see
[CONTRIBUTING.md](../CONTRIBUTING.md)).

```sh
export CONTROL_QA_SERVICE_IMAGE=sha256:<worker-image-id>
# Pause, fencing, explicit stop and confirmed removal.
nix develop --command cargo test --locked --bin control \
  services::tests::live_fenced_paused_service_can_be_stopped_without_restart \
  -- --ignored --exact --nocapture
# Retained /workspace across restart, exit, cleanup and replacement; foreign
# volumes rejected; unsupported quotas fail explicitly.
nix develop --command cargo test --locked --bin control \
  services::tests::live_workspace_survives_restart_exit_cleanup_and_container_replacement \
  -- --ignored --exact --nocapture
# Copy through the real dispatcher: pax paths, executables, skipped entries,
# service-user ownership, partial replacement and extraction conflicts.
nix develop --command cargo test --locked --bin control \
  services::copy::tests::live_copy_streams_a_tree_into_a_real_service_as_its_user \
  -- --ignored --exact --nocapture
# A real Control server: state across restart, lost Running receipt, expiry
# while Control is stopped, and the child cannot reach the supervisor.
CONTROL_QA_CONTROL_BIN="$PWD/target/debug/control" \
nix develop --command cargo test --locked --bin control \
  services::tests::live_service_survives_server_restart_and_expires_without_control \
  -- --ignored --exact --nocapture
```
