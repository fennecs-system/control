# HTTP admission

The main HTTP listener admits at most **32 handler threads** at once, including
long-lived routes. This is a transport bound, separate from model-worker slots
and agent scheduling. The existing **16 live-update stream** limit leaves half
the handler capacity available to other requests when only streams are open.

Admission happens before thread creation. There is no application request queue.
At capacity the listener makes one nonblocking write of a `503` response with
`Retry-After: 1`, then closes the socket. A nonreading peer, partial write or
socket error can prevent delivery of the complete response; the guarantee is
bounded rejection, not guaranteed error delivery. Clients should back off on
overload or a closed connection. Do not automatically replay mutations whose
outcome is uncertain.

Accepted requests retain the existing 10-second overall header/body read deadline,
3-second maximum individual read/write waits, 64 KiB header limit and route body
limits (at most 512 KiB). Reads also check shutdown between chunks, so trickling
bytes cannot defer cancellation until the full read deadline. Live updates use
the existing cancellation token and 15-second transport heartbeat. Completed
handlers release their slots; observed handler panics remain reportable during
shutdown. Already admitted tool operations keep their own operation-specific
waits and cleanup rules.

The cap bounds these handlers, not total process memory or every thread. Port
relays, preview transports, service supervision, response allocations and loaded
application state have separate resource costs. Saturating all 32 slots can
temporarily reject health checks and MCP requests too; there is no privileged
request lane. The limit is deliberately local to the HTTP adapter and adds no
persisted state, core effect, provider policy or agent wake.

Socket regressions live in `src/http_admission/tests.rs`. The isolated process
test in `tests/graceful_shutdown/http_admission.rs` covers authenticated live
streams alongside slow requests, H+1 overload, recovery, cancellation while
bytes continue arriving, a clean state save and zero model work. See
[performance evidence](PERFORMANCE.md) for release measurements and their scope.
