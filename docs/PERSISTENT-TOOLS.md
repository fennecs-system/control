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
a read-only judge.

The message and selected exact owner/generation grants commit
together, taking effect in the queued follow-up turn without interrupting or
changing an active attempt. Retries are idempotent; failed saves queue neither
the message nor the grants. A replacement service generation needs a new grant.
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
- `/workspace` and `/home/agent` use disk-backed container storage with no
  Control disk cap by default. Only written data consumes space; there is no
  up-front capacity reservation. Files survive model turns and Control restart,
  and remain until the container is removed. Attempt checkpoints do not include them.
  Shared base images remain immutable. Older services retain their captured tmpfs
  layout and limits; start a new service to use disk storage.
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
  New disk-backed service records require this version of Control; older
  binaries cannot read those records when rolling back node state.
- Eight reserved service slots per node, each limited to one CPU, 512 MiB RAM
  and 64 processes (Full: 2 GiB and 128 processes). Fenced resources retain their reservation.
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

Commands survive model turns and have a 1–1800 second timeout (default 120), capped
by the service lease. Poll `control_service_exec_status` until complete. One active
command per service and 16 retained full command outputs bound work and memory.
Evicted output leaves a lightweight retry receipt while its attempt can still
submit tools (at most 1,024 per service). Status reports `output_evicted=true`
when only that receipt remains. Retries return the recorded outcome rather than
re-executing; a full live receipt budget asks the worker to finish its turn.
Timeout, shutdown or uncertain completion requests service stop: killing the host
Docker client alone would not prove its remote writer stopped. A restart never
replays an in-flight command. Other services remain running. These tools provide
no host command execution, arbitrary container selection or new network authority.
Task-specific adapters and server configuration remain outside Control's source.

Use `/workspace` for task files and worktrees, `/tmp` for disposable scratch builds
and downloads, and `/home/agent` for tool configuration and caches. No `/scratch`
alias or host-private directory is needed. `/workspace` and home last until
container removal; `/tmp` is RAM-backed and disposable across container restarts.
Export anything worth keeping as an artifact or project checkpoint. A service
exit can trigger supervisor removal, so the writable layer alone is not crash
recovery or a backup.

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
