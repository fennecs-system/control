# Persistent tools

A service belongs to a chat and actor on one node. Model attempts can attach to
it across turns; finishing or cancelling a turn does not stop it. Explicit stop,
permission revocation, command exit or lease expiry starts cleanup. A service is
not another model session and does not consume a model-turn slot.

Delegated workers inherit captured grants for exact service generations. This
does not change ownership or expose other chats' services. Design continuations
retain grants; completed delegated work returns its resource handles to the parent.
Judges receive no service grants.

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
- Lite has a read-only root; Full has a private Nix layer. Both use a non-root user, dropped capabilities, no new privileges and
  a private PID namespace. No automatic container restart.
- Private writable tmpfs: `/workspace` 64 MiB (Full: 512 MiB), `/tmp` 128 MiB and `/home/agent`
  32 MiB. Files survive model turns and a Control process restart while the
  container stays alive; they are not durable storage across container stops,
  engine restart or machine restart. Attempt checkpoints do not include them.
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
command per service and 16 retained command receipts bound work and output.
Retries of the same request identity return its receipt rather than re-executing.
Timeout, shutdown or uncertain completion requests service stop: killing the host
Docker client alone would not prove its remote writer stopped. A restart never
replays an in-flight command. Other services remain running. These tools provide
no host command execution, arbitrary container selection or new network authority.
Task-specific adapters and server configuration remain outside Control's source.

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
leases and restoration after the model turn ends. An explicit ignored Docker
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
