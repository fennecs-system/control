# HTTP admission

The main HTTP listener runs at most **32 handler threads** at once
(`MAX_HANDLERS` in `src/http_admission.rs`), including long-lived routes. This is
a transport bound, separate from model slots and agent scheduling. Live-update
streams have their own limit of **16**, so streams alone never use more than half
the handlers.

Admission happens before a thread is created; there is no request queue. At
capacity the listener makes one nonblocking write of a `503` with
`Retry-After: 1` and closes the socket. A peer that is not reading, a partial
write or a socket error can prevent the full response from arriving: the
guarantee is bounded rejection, not error delivery. Clients should back off on a
`503` or a closed connection, and must not automatically replay mutations whose
outcome is unknown.

Admitted requests have:

- a 10-second overall deadline for reading the header and body;
- 3-second limits on each individual read and write;
- a 64 KiB header limit;
- per-route body limits: 512 KiB for `/save`, `/lenses/save` and
  `/knowledge/save`; 256 KiB for `/chats`, `/mcp` and chat messages; 32 KiB
  otherwise.

Reads check for shutdown between chunks, so a client trickling bytes cannot delay
cancellation. Live updates send a heartbeat every 15 seconds and stop on
shutdown. A finished handler frees its slot; a handler panic is reported when the
node shuts down.

The cap bounds HTTP handlers only, not total memory or every thread. Port relays,
preview transports, service supervision and application state cost resources
separately. Filling all 32 slots also rejects health checks and MCP requests;
there is no priority lane. The limit lives entirely in the HTTP adapter and adds
no persisted state.

Tests: `src/http_admission/tests.rs` covers the socket behaviour.
`tests/graceful_shutdown/http_admission.rs` runs an isolated process with live
streams and slow requests, overload, recovery, cancellation while bytes are still
arriving, a clean state save and no model work. Measurements are in
[performance](PERFORMANCE.md#web-http-and-chat-delivery).
