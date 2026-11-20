# Scoped TCP ports

[Persistent services](PERSISTENT-TOOLS.md) run with `network=none`. A named port is
a separate, operator-approved capability owned by one service, its container
generation, chat, actor and node. Control moves bytes through a trusted Rust relay
(`control-tcp-relay`) inside that exact, verified container. The service never
gets host or bridge networking, the Docker socket, host directories or
credentials.

## Approval

Allow or deny pending requests in chat, including requests from delegated agents.
In the TUI, use `/port allow ID` or `/port deny ID`; a literal loopback target also
needs `--loopback` when allowing. Approval lasts for the service's lifetime by
default and can be revoked; a shorter duration ends at the service deadline if
that comes first. The **TCP ports** section of the web tools page lists endpoints
and revocation controls. **Deny** works even without a valid duration. Neither
deny nor revoke stops the owning service.

Relays need a worker image whose `dev.control.tcp-relay` label matches the
version this Control build speaks (currently `5`). Services on older images are
refused with a rebuild instruction: rerun Nix setup, then start a new service.

## Tools

| Tool | Purpose |
| --- | --- |
| `control_port_request` | Record a request and return a handle at once. Grants nothing until approved. |
| `control_port_status` | Readiness, `local_address`, expiry and bounded diagnostics. |
| `control_port_stop` | Persist revocation of owned ports (`direction: "egress"` stops all egress at once). |

A chat cannot request or manage another chat's ports, and judges cannot manage
ports.

### Ingress

```json
{"service_id":"SERVICE_ID","name":"console","direction":"ingress","service_port":3000}
```

Once approved, ingress gets an ephemeral `127.0.0.1` port on the host, saved
before any connection is accepted. After a restart Control binds the same port;
if it is taken, the port fails closed rather than silently moving. A human TCP
client can connect to that address. Each ingress port accepts up to 32 concurrent
connections; further connections wait for a free slot.

Ingress is for **non-HTTP, non-TLS TCP protocols**. Serve HTTP pages and artifacts
with `control_preview_start` (see [workspaces](WORKSPACES.md#local-previews)).
Browsers share cookies across ports on a host, so raw ingress buffers the first
client bytes and rejects HTTP request lines (including custom methods and the
HTTP/2 preface) and TLS ClientHello before forwarding anything. Binary bodies
cannot get past the request-line check. An ambiguous initial ASCII line may be up
to 8 KiB and must complete within two seconds of its first byte; a newline or
write half-close marks it as non-HTTP. This can exclude protocols with long or
unterminated ASCII greetings. Server-first banners flow immediately.

### Egress

The service connects to its own named loopback port and Control forwards to one
approved target:

```json
{"service_id":"SERVICE_ID","name":"upstream","direction":"egress","service_port":3001,"target":{"host":"example.com","port":443}}
```

The operator approves that exact host, port and lifetime. Approval resolves the
host once, rejects mixed or special-use addresses, and pins one public IP.
Connections use only that IP and port, with no redirects or re-resolution. The
service can speak TLS itself; Control neither terminates TLS nor supplies
credentials. This is TCP forwarding, not container-wide egress; the
[egress proxy](#egress-proxy-https_proxy) admits only the same approved endpoints.

Service containers have no DNS. HTTP and TLS clients use the real host name
through the [egress proxy](#egress-proxy-https_proxy); non-HTTP clients dial
`127.0.0.1:service_port`, which relays to the target. Every egress record from
`control_port_request` and `control_port_status` carries this as `usage`, and the
approval wake repeats it. Redirects and other hosts need their own approved
endpoints.

#### Egress proxy (`HTTPS_PROXY`)

Package managers cannot pin SNI per host, so each service with at least one
approved target egress also gets one HTTP forward proxy at `127.0.0.1:3129`
inside its container. `control_service_exec` commands and newly started watches
then run with `HTTPS_PROXY`, `HTTP_PROXY`, `https_proxy` and `http_proxy` set to
`http://127.0.0.1:3129` and `NO_PROXY=localhost,127.0.0.1`. npm ci, pip, cargo,
git and curl use `HTTPS_PROXY` automatically for every approved host, with no
lockfile rewriting or task-local adapter. Commands
started before the first approval, or with no approved egress, get no proxy
settings; restart a watch to pick them up.

- `CONNECT host:port` is admitted only when the service holds an approved, live
  egress grant for exactly that host and port. Control connects to that grant's
  pinned address and then tunnels bytes; TLS stays end to end and is never
  terminated or inspected.
- A plain-HTTP absolute-URI request (`GET http://host/…`) is relayed only to an
  approved `host:80` grant, in origin form with `Proxy-*` headers dropped. Other
  ports are refused; use HTTPS.
- Anything else gets `403 Forbidden` whose body names the fix: request an egress
  port for that `host:port` with `control_port_request`. Malformed requests get
  `400`; an unreachable pinned target gets `502`.
- Revocation, expiry or a service change refuses new requests at once and closes
  open tunnels for that grant, like a port's own relay. The proxy stops when the
  service's last grant ends.

The proxy reuses the existing `listen-multi` relay (no image change): its streams
reach the host as data, which reads the request head (at most 8 KiB within ten
seconds) before connecting. The relay protocol still carries no destinations.
It shares the 32-session bound of one egress listener. Port 3129 is reserved:
`control_port_request` refuses it as a `service_port`. Peer routes are unchanged
and are not reachable through the proxy.

One egress listener carries up to 32 concurrent sessions. Each stream has its own
bounded flow control, so a slow download does not block other streams or
heartbeats. `ready` means the relay handshake succeeded and the listener is
running. It is live state and never saved, so after a restart it reads `false`
until the relay is back.

A literal `127.0.0.1` or `::1` target also needs the operator's explicit loopback
checkbox. Private DNS answers, metadata, multicast and LAN addresses stay denied;
the checkbox does not authorize a hostname that resolves privately. A loopback
grant exposes that exact node-local endpoint to the service, so check the target.

### Service-to-service routes

To connect two services you own or were granted, request ingress on the listening
service, then egress on the connecting service with
`"peer_port_id":"INGRESS_PORT_ID"` instead of `target`. Control pins the route to
that ingress listener, connects only while it is listening, and closes the route
when the ingress is revoked or moves, or its service changes generation. No
loopback checkbox is needed. The client dials its own `127.0.0.1:service_port`.
Services with different owner chats need access to both exact service
generations; the saved route captures that grant. A shared actor, root task or
guessed port ID grants nothing.

## Waiting for a decision

Do not poll a pending request. Its `next` field says to hand off with:

```json
{"disposition":{"kind":"wait","topics":["port:PORT_ID"]}}
```

A `port:` topic needs no subscription and is accepted only for a pending request
from this chat (an unknown one is dropped; an already decided one records
`continue` in a continuing bot, and elsewhere refuses the handoff so the agent acts
on the decision in the same turn). Naming only port topics is a narrow wait, not a
wait on every subscription.

When the operator decides, Control queues one decision turn in the requesting
conversation, bot or not, once it is idle. The turn lists each decision and, for
approved egress, its `usage`. A requester whose turn was cut off before its
handoff (time limit, local step cap or provider failure, which leave a host
checkpoint) is woken the same way. Each decision wakes its requester at most once,
from the Control process that recorded it. A decision that cannot be delivered is
retired: its requester ended without a checkpoint, the operator cancelled that
work (`/cancel`, or `/cancel all` here or in a parent chat) after the request, or
an approved port has since stopped. Decisions still undelivered at restart are
retired too, so a redeploy never resumes a conversation. An operator message
delivered into the requesting turn while it ran was seen by that turn, so it does
not count as later steering that turns the decision into a note. A persistent bot with a
pending request gets no automatic recovery turn; it sleeps until the decision.

When a `control_service_exec` or `control_service_exec_status` result shows a
name-resolution or connection failure and names the host of one of this chat's
egress requests, the result gets one hint: if pending, wait on `port:PORT_ID`; if
approved, use the proxy (restart a watch started before approval); if already
dialling the relay, check
`control_port_status` for `ready` and `diagnostic`. Other hosts get no hint.

## Limits and guarantees

- At most 16 active ports per chat's services and 256 per node. Invalid or
  expired pending requests retire and free their slot.
- Request nonces are scoped to the current attempt. Changed payloads under a
  reused nonce are rejected.
- Before each relay attach Control verifies the captured service ID, generation,
  full container ID, ownership, immutable image and offline scope. It never adopts
  a container by guessed name or label. Records from another node never restore
  listeners. Approved ports survive restarts.
- Revocation, service invalidation, expiry and shutdown close listeners and
  sockets. `stopping` is an intent; `stopped` confirms teardown. A TCP connect
  inside the service has a two-second limit. A peer may see closure as EOF or
  reset; graceful close is not promised.
- The relay runs as the service's non-root user, is nondumpable, and enforces its
  own lease. Without host heartbeats it exits after five seconds, bounding orphan
  listeners after a host crash. A slow reader does not stop heartbeats: data waits
  under bounded backpressure until it is read, access is revoked or the lease ends.
- Framing is shared std-only Rust. Frames are at most 16 KiB. A single-stream
  relay advertises one frame of credit; multiplexed streams up to 64. Duplicate
  credit or uncredited data fails closed. Control messages cannot carry commands
  or destinations. Write half-close is kept separate from transport EOF, and a
  normal relay exit drains final data and FIN frames. A versioned handshake
  rejects incompatible relays, including on restore.
- Control releases its state lock before Docker or network I/O and does no Docker
  polling when no ports are approved.

## Tests

Unit tests in `src/ports/tests.rs` cover nonce, owner and attempt checks,
failed-save rollback, endpoint pinning, loopback approval, expiry, generation
changes, stale requests, foreign-node restore, frame limits, split HTTP/TLS
prefixes, binary HTTP bodies and raw ASCII half-close. `src/ports/proxy/tests.rs`
covers the egress proxy against a local plain-TCP echo upstream: CONNECT
tunnelling (directly and through the multiplex transport), 403 with the fix for
unapproved hosts, plain HTTP only to port 80, revocation refusing new requests
and closing open tunnels, proxy settings present only with approved egress, the
usage text and the reserved port. The ignored `proxy_relay_benchmark` compares
tunnel throughput and round-trip latency with the raw relay path.

Two ignored live tests in `src/ports/tests/live_tests.rs` need
`CONTROL_QA_SERVICE_IMAGE` (an immutable image ID) and `CONTROL_QA_CONTROL_BIN`,
make no model calls and use isolated state:

- `live_tcp_ports_preserve_half_close_and_recover_after_host_death` checks both
  half-close directions, server-first banners, fragmented HTTP/TLS rejection,
  relay recovery after the host server is killed, stable ingress addresses,
  approval and revocation, lease expiry, a reader stalled for seven seconds that
  then receives an exact 16 MiB payload, revoking a backpressured stream, and
  service death.
- `live_service_commands_download_and_connect_two_private_services` (Full image)
  covers service commands, egress downloads and peer routes.
