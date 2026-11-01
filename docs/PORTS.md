# Scoped TCP ports

Persistent services stay on `network=none`. A named port is a separate,
operator-approved capability owned by one service, its container generation,
chat, actor and node. Control bridges bytes through a trusted Rust helper inside
that exact verified service. No host network, bridge network, Docker socket,
host directory or credentials are exposed to the service.

Open **Tools → TCP ports** to allow or deny a pending request, inspect the bound
service and endpoint, or revoke access. Approval lasts at most one hour and never
outlives the service's lease. The default is 15 minutes or its remaining lifetime.
Rebuilding worker images does not upgrade existing services: rebuild with Nix
setup, then start a new service to gain the trusted relay capability.

## Worker tools

`control_port_request` records intent and returns a handle immediately. It does
not grant network access. For example, request an ingress port on a running tool:

```json
{"service_id":"SERVICE_ID","name":"console","direction":"ingress","service_port":3000}
```

After approval, `control_port_status {"port_id":"PORT_ID"}` reports readiness,
`local_address`, expiry and bounded diagnostics. Ingress receives an ephemeral
host `127.0.0.1` port, persisted before accepting connections. After a Control
restart it binds that same port; an occupied port fails closed instead of silently
changing the endpoint. A human TCP client can connect to this address.

Ingress is for **non-HTTP, non-TLS TCP protocols**. HTTP pages and artifacts use
`control_preview_start`, which already provides the guarded web preview path.
Browsers share cookies across ports on a host, so raw ingress buffers and rejects
HTTP request lines (including custom methods and HTTP/2's preface) and TLS
ClientHello before forwarding client bytes. Binary HTTP bodies cannot override
the request-line check. An ambiguous initial ASCII line has an 8 KiB limit and
a two-second deadline after its first byte; a newline or write-half-close can
establish a non-HTTP payload. This restriction can exclude protocols with long
or unterminated ASCII greetings. Server-first banners still flow immediately;
the guard does not wait for client bytes before connecting the service.

For outbound TCP, the service connects to its own named loopback relay port:

```json
{"service_id":"SERVICE_ID","name":"upstream","direction":"egress","service_port":3001,"target":{"host":"example.com","port":443}}
```

The operator approves that exact host, port and lifetime. Approval resolves once,
rejects mixed or special-use addresses, and captures one public IP. Connections
use only that pinned IP and port, without redirects or DNS re-resolution. The
service can speak TLS itself; Control does not terminate or supply credentials.
This is TCP forwarding, not an HTTP proxy and not a container-wide egress grant.

After approval, a service command can preserve TLS hostname verification while
using the local relay, for example `curl --connect-to
example.com:443:127.0.0.1:3001 https://example.com/file -o /workspace/file`.
Redirects or dependencies on other hosts require their own approved endpoints;
package managers may need a task-local adapter rather than a generic proxy setting.

For two services, expose an ingress on the destination, then request an egress on
the source targeting that ingress's reported host `127.0.0.1` port. The operator
must explicitly allow loopback. The source connects to its own `service_port`;
the host port is the broker's target, not a container's direct route to the host.
Each side keeps its own ownership, readiness, revocation and lease checks.

A literal `127.0.0.1` or `::1` destination additionally requires the operator's
explicit loopback checkbox. Private DNS answers, metadata, multicast and LAN
addresses remain denied; checking the box does not authorize a hostname that
resolves privately. A loopback grant deliberately exposes that exact node-local
TCP endpoint to the approved service, so inspect the displayed target.

`control_port_stop {"port_id":"PORT_ID"}` persists revocation. `stopping` is an
intent; `stopped` acknowledges listener/connection teardown. Operator **Deny**
also works without a valid duration. Neither action stops the owning service.
A chat cannot request or manage another chat's ports; judges cannot manage ports.

## Lifecycle and bounds

- At most eight nonterminal ports per node and four concurrent ingress clients per
  port. One egress client at a time; a new helper listener is prepared after each
  disconnect. Applications should retry their local egress connection while it
  is being prepared.
- Request nonces are scoped to the current run/attempt. Repeated identical calls
  return the same handle; changed payloads are rejected. Captured service ID,
  generation, full container ID, ownership, immutable image and offline scope are
  verified before each relay attach. No guessed or label-only container adoption.
- Invalid or expired pending requests retire and free their slot. Foreign-node
  records never restore listeners. Approved intents survive Control restarts;
  data in the service remains bounded private container memory, not durable disk.
- Revocation, service invalidation, expiry and shutdown close listeners and active
  sockets. An in-progress TCP connect is bounded to two seconds and rechecked
  before forwarding. Engine inspection has the existing bounded Docker deadline.
  Externally observed TCP closure can be EOF or reset; it is not promised graceful.
- The helper is nondumpable, runs as the service's non-root user in the verified
  offline container, and enforces its own monotonic lease. Missing host heartbeats
  terminate it after five seconds, bounding orphan listeners after an abrupt host
  failure. A slow TCP reader does not suppress heartbeats or shorten the lease;
  data waits under bounded backpressure until the application reads, access is
  revoked, or the approved lease ends.
- The shared std-only Rust framing preserves TCP write-half-close separately from
  transport EOF. Frames are at most 16 KiB, the helper's queue holds one data
  frame. Each direction advertises one credit and returns it only after delivering
  the entire frame to its TCP socket. Parsers use nonblocking bounded queue sends
  and process heartbeats/credits independently of TCP writers. Duplicate credits
  or uncredited data fail closed. Heartbeat/control messages cannot carry commands
  or destinations. Both directions can finish independently. A normal helper exit
  drains final data/FIN frames before host teardown. The v2 immutable image marker
  and versioned handshake reject incompatible helpers, including on restoration.
- Control's state lock is released before Docker/network I/O. There is no polling
  Docker work when no ports are approved. Endpoints receive bytes only after
  approval; a service's normal offline execution boundary is unchanged.

New services also capture an executable private workspace policy, allowing
agent-authored tools to compile and run in `/workspace`. It is bounded tmpfs with
`rw,exec,nosuid,nodev`; the image root remains read-only and other tmpfs scopes are
unchanged. Older records missing `workspace_exec` retain their previous policy
and serialization. They are not silently upgraded or fenced by the new default.

## Verification

The unit tests cover nonce/owner/attempt checks, failed-save rollback, endpoint
pinning, explicit loopback approval, expiry, generation changes, stale pending
requests, foreign-node restore decisions, frame limits, split HTTP/TLS prefixes,
binary HTTP bodies, raw ASCII half-close, and legacy workspace compatibility.

The ignored `ports::live_tests::live_tcp_ports_preserve_half_close_and_recover_after_host_death`
test uses an isolated Control server, a Nix-built Rust echo service and a local
host endpoint. It checks both half-close directions, server-first banners,
fragmented HTTP/TLS rejection without forwarding browser bytes, active and idle
helper recovery after killing the host server, stable ingress addresses,
operator approval/revocation, lease expiry, a reader stalled for seven seconds
which then receives an exact 16 MiB payload, revocation of a backpressured stream,
and service death. It makes no model calls or external network requests. Supply
`CONTROL_QA_SERVICE_IMAGE` (immutable image ID) and `CONTROL_QA_CONTROL_BIN`.
