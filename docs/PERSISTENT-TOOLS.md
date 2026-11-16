# Persistent tools

A service belongs to a chat and actor on one node. Model attempts can attach to
it across turns; finishing or cancelling a turn does not stop it. Explicit stop,
permission revocation, command exit or lease expiry starts cleanup. A service is
not another model session and does not consume a model-turn slot.

Delegated workers inherit captured grants for exact service generations. This
does not change ownership or expose other chats' services. Design continuations
retain grants; completed delegated work returns its resource handles to the parent.
Judges receive no service grants.

Resources created after spawning can be shared explicitly through the existing
`control_agent_message` tool:

```json
{"chat_id":"<child chat>","text":"The service is ready; continue using it.","services":["<service ID>"]}
```

The sender must already have access. The recipient must be a same-actor
descendant or another worker under the same top-level spawned task. Siblings can
exchange targeted callbacks without a running parent, a root bot, or a room
broadcast. The steering/root chat is not a peer destination; consequential
operator decisions use `control_design_ask`. Joining a shared room alone does
not permit messaging unrelated task trees. Peer messages cannot bypass a bot's
pause/stop, revive cancelled or failed work, change its action scope, or continue
a read-only judge. An approved persistent bot that is not stopped is the one
exception for failed or interrupted turns (a transient provider error, for
example): its turn ended, not the bot, so the message is retained for the wake
its supervisor already owns. The message never starts or restarts a run itself.
Stopped bots (`/bot off`) and cancelled work still refuse it.

The message and selected exact owner/generation grants commit together without
interrupting or changing an active attempt. Descendant follow-ups queue as before.
Peer messages return `queued:false, retained:true`: active recipients read the
typed conversation evidence; idle recipients need an approved persistent bot to
admit a wake under its existing budget and pause/stop intent. No root bot is
required. Unapproved or stopped recipients gain no scheduling authority.

Peer admission waits until the recipient is idle and published processing receipts
have been reconciled. Consumed messages do not create another turn. A grant offer
still needs the next authorized attempt to activate its exact resource generation;
reading its text does not prematurely retire that permission transition. Completed
peer evidence uses the existing history journal rather than growing inline copies
of context in each checkpoint. Retries are idempotent; archive/checkpoint failures
roll back the message, receipt and grants. A replacement service generation needs a new grant.
Omitting `services` transfers no additional authority. A service grant permits
the existing service operations, including execution and stop; it is not a
port-only permission. Scoped TCP approval remains a separate operator decision.

Use peer messages for requests and answers that one worker needs to proceed;
discover peer chat IDs from the linked room's participant list. The recipient
retains its own task, model, context and grants. Messages are coordination evidence,
not new operator steering. Replies remain in the recipient's chat: send an answer
directly when the sender is blocked on it. Do not also post the same request or
answer as a room notification. The steering chat can finish after assigning
ownership; it does not acknowledge or relay each worker update.

## Task coordination threads

New tasks coordinate in a thread under `general`, `engineering` or `research`.
The stable scope is the top-level chat, not a provider attempt: workers spawned
before or after room attachment resolve the same thread, and continuations keep
it across restart. A worker using `room: "general"` reads and posts within that
task's thread. Unrelated task history, chat links and callbacks are excluded.
Threads reuse the existing room records, inbox cursors and event subscriptions;
they add no supervisor state, periodic polling or deadline.

The Rooms menu lists task threads by their lead's title. Operators can read and
post into a selected thread or explicitly create another chat there. Exact room
addresses are `<channel>/<thread chat ID>`; worker tools cannot choose another
task's thread. Existing legacy shared-room attachments remain shared so an
upgrade does not silently move an active swarm or invalidate its receipts. To
move an old chat, disable its bot/listener, then use `/room general` (or another
channel) to attach its task thread. Prior shared history remains available to
the operator; new threads do not import it.

## First slice

The worker MCP surface advertises these tools for ordinary Codex and Claude
attempts. The Tools page uses the same discovery catalogue. Judges cannot call
them. Operator stdio/HTTP MCP does not yet expose service management.

| Tool | Result |
| --- | --- |
| `control_service_start` | Checkpoint a command and return its durable handle; startup is asynchronous. |
| `control_service_status` | Inspect one handle, or list active services and ten recent completed handles. |
| `control_service_attach` | Read status and bounded output for an owned handle; does not extend its lease. |
| `control_service_stop` | Record stop intent. Wait for `Stopped` before assuming removal succeeded. |
| `control_service_exec` | Submit a bounded command inside an owned running service; returns an execution ID. |
| `control_service_exec_status` | Read that command's live output, durable status and exit code. |
| `control_service_copy` | Copy a file or tree from the task workspace into an owned running service's `/workspace`; returns an execution ID. |

Commands run in a fresh container from the attempt's captured, immutable Nix
worker image. Rebuild worker/Claude images with the documented Nix setup before
using these tools: older images lack the trusted `control-service-runner`.
No image is modified or committed from an agent's writable state.

The initial scope is deliberately small:

- No Docker network, provider credentials, Docker socket or host/attempt mounts.
  [Scoped TCP ports](PORTS.md) require separate operator approval and trusted
  relays; they do not enable the container's network namespace.
- New Lite and Full services have a private writable container layer. Both use a non-root user, dropped capabilities, no new privileges and
  a private PID namespace. No automatic container restart.
- New services mount `/workspace` from a retained local Docker volume owned by
  the node and chat. Replacement services in the same chat automatically reopen
  that workspace, including after process exit, container cleanup or Docker
  restart. Same-chat services share files and must avoid conflicting writes;
  other chats and nodes use different volumes. Names are derived by Control,
  never accepted as tool input. Only written data consumes space; there is no
  up-front reservation or Control disk cap. Attempt checkpoints do not include
  these files, and retained storage is not a backup against disk loss.
  `/home/agent` remains container-local configuration/cache storage and is lost
  on removal. Shared base images remain immutable. Existing services retain their
  captured container-layer or tmpfs layout; they are not silently migrated.
  `status.workspace` identifies retained storage; null means the legacy layout.
- `/tmp` remains a 128 MiB RAM-backed tmpfs for disposable scratch. Its contents
  disappear on container restart and count against the existing memory limit.
  Larger temporary builds and downloads can use a scratch folder in `/workspace`.
- Optional `storage_limit_mb` on `control_service_start` asks Docker to enforce a
  private writable-layer quota in MiB. It excludes separate mounts, including
  approved project folders, and never measures or limits the host source folder.
  This requires a quota-capable storage driver (for example, overlay2 on XFS with
  project quotas). Unsupported caps fail service creation with a diagnostic;
  Control never silently drops them. The default works without quota support.
  The engine's available disk and any daemon-wide limits still apply.
  New service storage records require this version of Control; older
  binaries cannot read those records when rolling back node state.
- The node service budget below, not a fixed count, bounds how many services
  run (16 default-shape services with the default budget). A ceiling of 128
  live service records keeps state and engine inventory bounded. Services are
  not tied to agents or bots: many agents can share one service, whose commands
  run concurrently, or spread across several. Omit `shape` to keep the image default:
  one CPU, 512 MiB RAM and 64 processes (Full: 2 GiB and 128 processes).
  Optional compute presets are independent of Lite/Full image capabilities:

  | `shape` | RAM limit | CPU limit | Process limit (default) |
  | --- | --- | --- | --- |
  | `small` | 2 GiB | 1 | 128 |
  | `medium` | 4 GiB | 2 | 256 |
  | `large` | 8 GiB | 2 | 512 |

  Preset identifiers have immutable limits and are saved with the service.
  Existing records without a shape keep their original limits and PID policy.
  The process (pids cgroup) limit counts threads, so JVMs and multi-client Node
  workloads need more of it as RAM grows. It is `service-pids-per-gib` (default
  64) per GiB of the RAM limit, capped at `service-pids-max` (default 1024) and
  never below the image default (Lite 64, Full 128). Control captures the value
  when the service is admitted and uses that same value to create the container
  and to check it afterwards, so changing these settings affects only new
  services. Services created before process limits followed RAM keep their
  original image-default limit and are not fenced; replace one to get the
  larger limit. `pids_limit` appears in service status and on the services page.
  When a service reaches its limit, new commands fail with
  `Resource temporarily unavailable`; stop extra processes or replace the
  service with a larger shape.
  The node's service pool admits services atomically with the durable
  reservation. Memory is a strict sum of limits, 20 GiB by default. CPU limits are
  caps rather than reservations, so CPU is admitted against the node's CPUs times
  an oversubscription factor, 8 x 2 = 16 by default; each container keeps its hard
  CPU cap and excess demand is throttled. Operators size the pool with
  `service-memory-gib`, `service-cpus` and `service-cpu-oversubscription` (1-8)
  in the node configuration directory, and set process limits with
  `service-pids-per-gib` and `service-pids-max`. Pending, stopping and fenced resources count;
  only confirmed teardown releases capacity. An over-capacity request fails
  without creating a service; retries do not reserve twice. This is a service
  admission budget, not a host allocator: external model servers, worker
  containers and Control itself require additional headroom. Docker's physical
  capacity may be smaller; these limits do not guarantee resident memory or
  dedicated CPU cores. Unsupported engine limits fail visibly, never uncapped.
  `control_service_status` without an ID returns presets and remaining budget.
  Each service reports its shape and effective `limits` (bytes and CPU count).
  The Services page shows these limits separately from observed live usage.
  Stopped services can be deleted there one at a time, or all at once with
  **Clean up stopped services**. Deletion removes the stopped records and, once no
  service record of that chat remains, the chat's retained `/workspace` volume;
  only volumes carrying this node's Control labels are removed, and the engine
  refuses one still mounted. Running services must be stopped first.
  Start with `{"command":"exec your-server", "shape":"medium"}` for 4 GiB,
  or `"large"` for 8 GiB. Allow room inside the limit for application overhead;
  a managed runtime's heap must be smaller than its container memory limit.
  Resizing requires stopping and replacing the service in the same chat, which
  reopens `/workspace`; ports and grants must bind the new service handle.
- `lease_seconds: -1` (default) retains a service until stopped; use 1–3600
  seconds for a bounded lifetime. Use a foreground command; shell exit ends
  the service and its descendants. Port approvals default to the service's
  lifetime and remain revocable; an optional shorter duration never outlives it.
- Per-container Docker logs rotate at two 1 MiB files. Reads return at most
  80 lines and 16,000 bytes. Completed history is bounded to 128 records;
  stopped receipts remain pinned while their originating attempt is active.
  Final bounded output and an observed exit code are saved before removal, so a
  short failed probe remains inspectable after its container disappears.

Attachment reads logs and metadata. To configure a service after approval, use
`control_service_exec` with its service ID. Shell commands and heredocs can write
private setup files; clients can use the service's approved loopback relays to
download binaries directly into `/workspace`, without passing them through model
text or mounting a host folder. The service's foreground process must stay alive;
`exec sleep infinity` keeps an idle worker alive while setting up other processes.
Long-lived background programs must redirect stdin/stdout/stderr to private files.

### Copying task files into a service

`control_service_copy {"service_id", "from", "to"}` brings a reusable tool, such
as a project folder opened with `/open`, into a service without a host mount or
exec heredocs. `from` is relative to the attempt's task workspace (`/workspace`
in its run container, approved project folders included); `to` is relative to
the service's `/workspace`. A leading `/workspace/` is accepted on either. Every
segment must be a plain name: no absolute paths, `..`, `.` or empty segments.
The tool accepts no host paths and results never contain them.

- **Authority.** Only the caller's active, non-judge attempt can copy, and only
  from its own workspace, into a service it can already exec in: the same chat
  ownership, actor, node and exact-generation grant checks as
  `control_service_exec`. Judges are denied at the tool gate as well.
- **Source safety.** Host-side reads walk pinned directory descriptors with
  no-follow opens, reusing the artifact publication path policy. The source and
  its parents must be real directories or a regular file. Inside the tree,
  symlinks (dangling ones too), FIFOs, sockets, devices, non-UTF-8 names and
  credential, Control-internal or generated names (`.git`, `.env*`, `auth.json`,
  `.claude`, `.codex`, `.control-*`, `target`, `node_modules`) are skipped
  without being followed. They are counted, with up to eight examples, in the
  result. Depth is limited to 32, like publication.
- **Tool packages.** Admitted tool packages ([TOOL-PACKAGES.md](TOOL-PACKAGES.md))
  appear at `/workspace/.control-tools/<name>` and are the one Control-internal
  source a copy may read, with the same link, special-file, name and bound rules
  for the rest of the path and tree. Other `.control-*` sources (mailbox, room
  and result files) stay refused, and no destination may use a `.control-*` name.
- **Bounds.** The whole tree is checked against the byte and entry bounds
  before admission and again before launch, so an oversized source writes
  nothing. Streaming enforces them a third time. Defaults are 256 MiB of file
  content and 20,000 files plus directories; operators can set
  `service-copy-max-mib` (1–8191) and `service-copy-max-entries` (1–1,000,000)
  in the node configuration directory. A memory-backed or capped legacy
  `/workspace` lowers the byte bound to its size. A retained workspace volume
  has no Control cap (`storage_limit_mb` excludes it); the engine's disk still
  applies, and running out fails the copy.
- **Streaming.** Control writes a minimal ustar/pax archive (directories and
  regular files only, executable bits kept) straight from each open file into
  `tar -x` run by `docker exec --interactive` as the service's uid/gid in
  `/workspace`. Nothing is buffered beyond a 64 KiB writer. Files are owned by
  the service user, and the copy can write nothing that user could not already
  write with a command. Existing files at the same paths are replaced and other
  files are kept. A file that shrinks while streaming fails the copy; growth
  after it was opened is not copied.
- **Exclusive.** A copy takes the service exclusively. Admitted while commands
  run, it stays `requested` (its diagnostic says how many it waits for) and
  launches alone once they finish; from admission until it ends, new commands
  and copies are refused with the copy's `execution_id`. No command observes a
  partial copy and a copy cannot race a build reading the same files. Waiting
  is bounded by the running commands' timeouts. It reuses the command
  machinery: durable intent before launch, request-identity receipts (a retried
  nonce returns the recorded outcome without rescanning or recopying, and a
  reused nonce with different arguments is refused), the timeout
  (`timeout_seconds`, default 300) and in-service reaping on timeout or
  interruption, and restart recovery that never replays it.
  `control_service_exec_status` reports progress, a summary and skipped entries.
- **Failure.** The source is read only while the requesting attempt is active.
  Finish polling before ending the turn: a copy dispatched after its attempt
  ended, or whose source vanished or grew past a bound, fails before anything
  enters the service. Extraction failures such as a full disk or a file over an
  existing directory mark the copy failed without stopping the service. Copies
  are not atomic: a failed or interrupted copy may leave earlier files written.

The reverse direction (service to task workspace) is deliberately not provided.
It would extract untrusted service output into a host directory: a new
host-write boundary that needs its own safe extractor, interaction with the
checkpoint barrier and node storage accounting. Until such a capability is
separately reviewed, small results can be read back through bounded command
output.

Commands survive model turns and have a 1–1800 second timeout (default 120), capped
by the service lease. Poll `control_service_exec_status` until complete.
Agents sharing a service run commands concurrently, each with its own
`execution_id`, request identity, timeout, output and outcome. The service's
resources are the bound: every command is at least one process under its
`pids_limit` (which grows with RAM) and shares its memory limit, so a service
admits at most `pids_limit` active commands; use a larger shape for more. The
node setting `service-commands-max` (default 64, 1–1024) is only a safety
ceiling on Control's side: each running command holds a supervisor thread and
an engine client, and its persisted record is at most about 18 KB (a 2,000
character label plus 16,000 bytes of output; the command text is dropped from
state at launch). At the limit a new command is refused before submission with
the active `execution_id`s, their owning chats and commands, rather than
queued. Up to 16 finished outputs (about 18 KB each at most) are retained beside
the active commands, so a service's command records stay below
`(ceiling + 16) x 18 KB` (about 1.4 MB by default) however many commands ran.
Evicted output leaves a lightweight retry receipt while its attempt can still
submit tools (at most 1,024 per service). Status reports `output_evicted=true`
when only that receipt remains. Retries return the recorded outcome rather than
re-executing; a full live receipt budget asks the worker to finish its turn.
A timed-out or interrupted command is stopped inside the service together with
every process it started, detached ones included; the service keeps running.
Only when that cannot be verified is the service stopped: killing the host
Docker client alone would not prove its remote writer stopped. Run long-lived
programs such as clients, servers or daemons as a permanent watch
(`control_watch_start`, see [WATCHES.md](WATCHES.md)) instead of a command.
Use `restart=transient` for finite work such as large downloads: it runs once,
even when it takes longer than a model turn. Both watch modes have no task
deadline; `restart=permanent` restarts after any exit, including success. The
program must itself stay connected between actions if continuous participation
is required; supervising it does not prevent deliberate disconnects or exits.
A restart never replays an in-flight command. Other services remain running. These tools provide
no host command execution, arbitrary container selection or new network authority.
Task-specific adapters and server configuration remain outside Control's source.

Keep task programs, worktrees, downloads and durable application data under
`/workspace`. Commands start there by default; configure applications that would
otherwise write to `/home/agent` or elsewhere to use a workspace data directory.
This is a storage contract and tool instruction, not a filesystem write jail:
applications can still write elsewhere, but those files are not retained.
Use `/tmp` for disposable scratch and `/home/agent` for disposable configuration
and caches. No `/scratch` alias or host-private directory is needed.
Export selected results as artifacts or project checkpoints; those have separate
publication limits and do not replace service storage.

Container removal deliberately does not remove workspace volumes. To inspect
retained volumes, an operator can run
`docker volume ls --filter label=dev.control.purpose=service-workspace` and inspect
their `dev.control.node` and `dev.control.chat` labels. Service status also gives
the exact volume name. Only delete a volume explicitly after confirming its data
is no longer needed; Docker volume pruning can destroy retained workspaces.
Control does not automatically restart application processes or replay commands
after Docker restarts. Start a replacement service and inspect its existing
workspace before launching the application again. This does not recover files
already lost from legacy container layers.

## Lifecycle and recovery

`control-core::resources` contains validated IDs, ownership, generation, lease,
scope and a pure transition function. Time, observations and authority enter as
inputs. It returns a phase and explicit effect; Docker and storage remain in
application adapters. Default core builds require only `std`; persistence uses
its optional Serde feature and validated deserialization.

The application persists create intent before creating a container, then records
its exact ID before starting a command. A lost creation receipt can adopt only a
dormant container with matching identity and scope. A missing Running receipt
can reconcile the already-bound ID without starting it twice. An exited or
missing command is never automatically replayed.

The supervisor checks the immutable image, node/chat/resource/generation, exact
container name and known ID, command/watchdog arguments and enforced scope before
adopting, starting, removing or reading output. Labels alone grant no authority.
Start/removal adapters accept a verified binding. A mismatch fences the record
for operator inspection and pauses ordinary dispatch if reconciliation cannot
account for the container; it does not delete an unfamiliar container.

Stop intent precedes removal, and only an observed absence completes cleanup.
An explicit stop also retries cleanup of a fenced service. It keeps the recorded
container identity and reservation until the engine confirms absence; an absent
entry in label-filtered inventory is checked by exact container ID as well.
An owned paused container can be removed after identity and scope verification.
Mismatches remain fenced, and engine or persistence failures retain the reservation.
Fenced services never restart automatically or resume their command during cleanup.
An engine failure remains pending with a bounded diagnostic. Revocation permits
cleanup but prevents new create/start effects. No exactly-once external effect
guarantee is implied by this local reconciliation protocol.

A trusted Rust PID 1 enforces the recorded lifetime (a monotonic deadline for timed services),
even while Control is stopped. It starts non-dumpable before spawning the command
and reaps descendants. Linux terminates the PID namespace when its init exits;
same-namespace children cannot send an unhandled SIGSTOP/SIGKILL to PID 1.
See [PID namespaces](https://man7.org/linux/man-pages/man7/pid_namespaces.7.html)
and [PR_SET_DUMPABLE](https://man7.org/linux/man-pages/man2/PR_SET_DUMPABLE.2const.html).
This depends on the Linux container boundary and a trusted engine/host. It is not
a VM isolation claim. Host/engine clocks should be synchronized; a separate
maximum duration bounds the watchdog even if clocks differ.

## Verification and cost

The worker MCP mailbox handles both individual JSON-RPC requests and batches.
Batch elements execute in order through the same attempt authorization checks,
with separate delivery identities; notifications produce no JSON-RPC reply.
Versioned bridge deliveries correlate host replies by nonce, including batches
and notifications. Older pinned bridges keep their original reply format.
The host claims a mailbox request before executing it, so a late reply cannot
delete the next request. A missing reply never automatically replays an effect.
Transport errors distinguish a request that was not sent from an uncertain
outcome that must be inspected before retrying.

The bounded `.control-room-mcp.log` records exchange steps and tool names, without
arguments or results, alongside the latest-step `.last` file. These internal
files are excluded from published workspaces. They distinguish a provider that
has not sent a call from a host that has not answered it.

Native fault tests cover checkpoint failures, duplicate/replayed requests,
bounded retention, ownership/scope mismatches, permission revocation, expired
leases and restoration after the model turn ends. They also cover capacity
recovery from fenced records, failed cleanup checkpoints and containers hidden
from label-filtered inventory. An isolated Docker regression exercises pause,
fencing, explicit stop and confirmed removal without any model calls:

```sh
CONTROL_QA_SERVICE_IMAGE=sha256:<worker-image-id> \
nix develop --command cargo test --locked --bin control \
  services::tests::live_fenced_paused_service_can_be_stopped_without_restart \
  -- --ignored --exact --nocapture
```

The storage regression writes a 540 MiB file as the service's non-root user,
checks restart, then stops the container and lets Control's supervisor remove
it. A new service handle under the restored chat owner must read the retained
file, while container-local home and scratch files are gone. It also rejects a
foreign-labelled volume before container creation and verifies quota support
fails explicitly rather than silently dropping the requested limit.

```sh
CONTROL_QA_SERVICE_IMAGE=sha256:<worker-image-id> \
nix develop --command cargo test --locked --bin control \
  services::tests::live_workspace_survives_restart_exit_cleanup_and_container_replacement \
  -- --ignored --exact --nocapture
```

The copy regression admits copies through the real dispatcher into a running
service: a 3 MiB tree with a long (pax) path, an executable and a skipped
symlink and `.env`. It checks service-user ownership and that the copied program
runs, then that a later copy replaces one file and keeps the rest. Finally, an
extraction conflict fails without stopping the service. It writes state and the
attempt workspace under the isolated `CONTROL_STATE_DIR`:

```sh
CONTROL_QA_SERVICE_IMAGE=sha256:<worker-image-id> \
CONTROL_STATE_DIR=/private/tmp/control-copy-qa \
nix develop --command cargo test --locked --bin control \
  services::copy::tests::live_copy_streams_a_tree_into_a_real_service_as_its_user \
  -- --ignored --exact --nocapture
```

An explicit ignored Docker
test starts an isolated real Control server, preserves a private counter across
restart, recovers a lost Running receipt and verifies expiry with Control stopped.
It also tests that a child cannot open its supervisor's memory or stop its timer.
No provider login or model call is used.

```sh
CONTROL_QA_SERVICE_IMAGE=sha256:<new-worker-image-id> \
CONTROL_QA_CONTROL_BIN="$PWD/target/debug/control" \
nix develop --command cargo test --locked --bin control \
  services::tests::live_service_survives_server_restart_and_expires_without_control \
  -- --ignored --exact --nocapture
```

Idle supervision performs no engine queries or state writes. Active resources
share one batched inventory per pass; unchanged Running observations do not save
state. Snapshots share command text through `Arc<str>` and omit completed history.
The existing `control.rust.bench` MCP tool measured the pure Running/Tick decision
at a median 12.5 ns on the development node (optimized Rust, 100,000 iterations,
nine samples). This measures the reducer only, not container startup or an
improvement over a previous persistent-service implementation.

Next: a pinned tool-runtime request contract and cooperating agents using their
own tools. Operator-approved TCP access is described in [scoped ports](PORTS.md).
Durable storage, service restart policy, bot callbacks and broader attachment
are separate increments. See the [roadmap](../roadmap.md) for demonstration goals.
