# Library boundaries

Keep the application understandable through small interfaces and independently
tested libraries. Extract an existing responsibility when the boundary removes
coupling or repeated work; moving files alone does not create that boundary.

Current libraries:

- `control-context`: deterministic captured context, lens composition and shared
  command definitions, independent of mutable application state.
- `control-http`: typed HTTP response construction and security/header policies.
- `control-ai`: provider record decoding, normalized
  activity, usage and final results. No credentials, networking, containers or UI.
- `control-core`: borrowed scheduling inputs, validated slot capacity, pure
  admission/dispatch/attempt-generation predicates and typed persistent-resource
  transitions. Default builds use only `std`; optional Serde support validates
  identities and leases at persistence boundaries. Full application commands,
  authority checks, durable writes and Docker supervision remain in adapters.
  See [persistent tools](PERSISTENT-TOOLS.md) for the current resource contract.
- `control-effect`: `std`-only. `machine` defines the `Machine` trait (event and
  supplied time in, stamped and durability-tagged effects out) with no clocks,
  I/O or locks. `sync` provides `CancelToken`, a bounded cancellable channel and
  a `Changes` generation counter, so drivers wait instead of polling. A durable
  checkpoint does not make an effect eligible; interpreters revalidate its stamp
  first. Its API follows tokio's shapes, so an async driver can replace a
  blocking one without changing machines. Not a scheduler or an event bus.
- The `control` library currently shares configuration, identity, diff rendering
  and the Discord adapter between executables. Discord is compiled once rather
  than included as source in two binaries. It consumes a typed delivery projection
  instead of retaining the entire application's JSON tree. Authorization and DM
  binding are rechecked at delivery; the projection never grants authority.

The next useful splits are `control-tui` for the existing MCP client and terminal
interface, and `control-web` for HTTP/UI adapters. Their dependency should be a
small application interface, not direct access to every mutable runtime field.
The TUI is already close to this boundary; web routing and rendering still need
their state projection and authorization calls made explicit.

These are proposed boundaries, not completed extractions: the terminal currently
includes `src/harness.rs` in two binaries, and most server state, routing and
rendering are still declared by `src/main.rs`. The Rust/WASM package under `web/`
is the browser client, not yet a `control-web` server adapter.

MCP is another adapter, not core business logic. A future `control-mcp` should
own JSON-RPC/protocol negotiation, tool discovery schemas and protocol error
mapping, calling the same application operations as web and terminal clients.
The shared tool catalogue already exists; do not implement a second tool system.
Extract that crate after the application interface is real rather than exporting
the whole state to an otherwise cosmetic protocol crate. HTTP and stdio can share
its protocol logic while retaining their own framing and authentication.

Use `control-web` for the server-side web adapter and `control-web-ui` if/when
renaming the existing Rust/WASM browser client is useful. These components can
run in one binary and call each other directly; separate crates do not require
separate processes, network hops, a message bus or repeated JSON serialization.

The application interface should expose typed commands and bounded queries:
create/send/steer a conversation, request a project, cancel a run, inspect a run,
and read conversation updates. HTTP/MCP and local web handlers must invoke the
same authorization and durable application operations. The TUI remains an MCP
client. An in-process web adapter should call that application interface directly;
it does not need loopback HTTP, JSON serialization or copied history to act like
a client. Remote clients can serialize those same contracts through an admitted
transport. Credentials and authority are checked on the serving node.

Keep render-only work outside the mutable-state lock where possible. Use borrowed
views within a request and bounded owned snapshots when crossing a task/thread
boundary; do not export `Arc<Mutex<State>>` as the public core API. Preserve the
existing checkpoint ordering and result fences during extraction. Full transcript
clones or a new event bus are not prerequisites for this separation.

`control-core` should contain durable run transitions, admission, resource
ownership and lifecycle rules. It should not become a catch-all for provider
protocols, web rendering, Discord delivery or tool implementations. Concrete
process/container executors interpret effects; adapters present results. Provider
selection and normalized events do not grant filesystem, network or tool access.
Future direct model APIs and local endpoints should use explicit adapters, rather
than pretending a Codex/Claude CLI harness is the same protocol as a model API.

Keep these as incremental extractions with persistence/authorization regressions.
Measure targeted rebuilds before claiming compile-time improvements. Avoid an
umbrella plugin runtime, cyclic dependencies and a crate for every source file.

Persistent tools should drive the next core extraction: typed resource identity,
ownership/generation, lease and lifecycle transitions, plus the narrow operations
needed to start, inspect, attach and stop. Pure decisions return explicit effects;
the application checkpoints intent and container adapters perform those effects.
Keep engine inspection, network enforcement, clocks and storage outside core.
Model attempts attach to a resource; they do not implicitly own its whole lifetime
or gain writable access to another attempt's checkpoint.

Do not wait for the full web/TUI split to implement the first persistent tool.
Reuse command-job and preview mechanisms where their contracts match, keeping
attempt-bound jobs and independent services distinct. Extract a common executor
only when actual shared behavior supports it; neither duplicate a second
scheduler nor introduce a generic orchestration framework for the first service.
Record warm targeted check/rebuild times, transcript render time, lock-held work
and allocations/peak memory on fixed fixtures before and after each slice. A
crate split alone does not improve scheduling capacity or establish remote-node
ownership, authenticated transport, replay protection or distributed consistency.

## Delivery reader measurement

An ignored, credential-free benchmark compares the old full JSON-tree reader,
a streaming typed reader, and the chosen typed slice decoder. Run:

```sh
nix develop --command cargo test --release --locked --lib \
  discord::delivery::tests::profile_delivery_projection -- --ignored --exact --nocapture
```

On the development Mac, three fresh-process trials of ten decodes each on a
10,253,422-byte synthetic ledger (250 runs, 750 attempts, 7,500 activities):

| Reader | Median elapsed / 10 decodes | Peak process RSS |
| --- | --- | --- |
| Previous full JSON tree | 52.9 ms | 30,240 KiB |
| Typed streaming | 222.1 ms | about 3,520 KiB |
| Typed slice, size-reserved buffer | 29.7 ms | 13,488 KiB |

The selected reader uses about 44% less decode time and 55% less peak RSS in this
fixture. Its temporary byte buffer is released after decoding; only delivery
fields, the latest attempt and its last relevant file change are retained.
Unchanged polls reuse the cached projection. The validated file reader reserves
the already-checked descriptor size once, avoiding repeated growth/copies.
Streaming consumed less memory but cost substantially more parsing time here.
These are warm-file synthetic cache-miss measurements, not end-to-end latency or
a guarantee for every ledger. The benchmark remains available to check the tradeoff.
