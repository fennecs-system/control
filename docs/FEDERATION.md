# Control federation — specification 0.1 (review revision 1)

Status: first slice implemented in `src/federation.rs`, reviewed by an author and adversarial reviewer. This is iterative design review, not model training. The first implementation is a deliberately bounded exchange of immutable context resources. Remote execution is specified separately and MUST remain disabled until its prerequisites are implemented and verified. The review exchange is recorded in [the review log](../reviews/2026-10-28-federation.md).

## Purpose and foundations

Control connects independently operated nodes. A node hosts identities, conversations, documents, pure lens definitions, agent state, and a deterministic execution supervisor. Federation lets an operator share selected resources or delegate bounded work without making another node its operator.

ActivityPub separates client interaction from directed server-to-server delivery through inboxes and outboxes. Control borrows explicit recipients, stable activity identifiers, and deduplication rather than treating every peer as a shared process. See [W3C ActivityPub, server-to-server interactions](https://www.w3.org/TR/activitypub/#server-to-server-interactions).

AT Protocol separates durable identity from human-readable names and service location, and uses signed, content-addressed repositories. Control borrows the separation of identity, location, immutable content, and application views. See [AT Protocol identity](https://atproto.com/guides/identity), [data repositories](https://atproto.com/guides/data-repos), and [stack architecture](https://atproto.com/guides/the-at-stack).

These are design foundations, not wire compatibility. Control 0.1 does not implement ActivityPub, DIDs, AT repositories, relays, public discovery, consensus, or a global ledger. A public social firehose is a poor default for private source code and chats.

## Invariants

1. Every node is independently administered. A peer or its agent is never a local operator merely by joining.
2. An authenticated identity proves who sent bytes. It does not prove their truth, safety, or authority.
3. Effective capability is the intersection of issuer delegation, authenticated peer scope, local policy, resource policy, and run constraints. Lenses select context/tool descriptions and grant no capabilities.
4. Private chats, source trees, credentials, operator cookies, and role bindings are never replicated implicitly.
5. An imported object is an immutable foreign resource. It cannot overwrite a local document/lens, activate a tool, install a UI, start a run, or execute Nix.
6. State transitions, authorization, retries, deadlines, quotas, and cleanup are deterministic code. Agents may propose plans and supervise meaning; they cannot adjudicate or bypass these mechanisms.
7. Federation remains usable without an always-running model. Idle persistent agents are durable identities/mailboxes/personas, not continuously generating tokens.

## First delivery: signed resource exchange

The initial transport is explicit operator-mediated file exchange between two nodes, suitable over an existing trusted channel. It demonstrates authentic, selective, revocable acceptance of a context snapshot without exposing the laptop's HTTP operator interface. It is a foundation for federation, not a claim that live mesh networking exists.

Each node has an Ed25519 signing key generated from OS cryptographic randomness, stored in a private node-state directory, never in the source repository or worker mounts. Its node ID is `ed25519:` followed by the lowercase hex encoding of the 32 public-key bytes. Labels and network locations are separate metadata. This identity is key-bound: changing the key creates a new identity in 0.1. Secure rotation with continuity and endpoint discovery are future protocol work, not TOFU exceptions.

Operator pairing pins an exact peer node ID with a local display label, inbound allowlist of resource kinds and root lens IDs, and a distinct permission to receive included document bodies (off by default). A wildcard root grant, if supported, must be explicit; kind alone does not authorize every resource on the sender. This root grant permits the bounded transitive definitions and explicitly permitted documents required by that root, not unrelated bundled records. Self-pairing is rejected. There is no automatic reciprocal trust and no transitive trust. A claim that another node trusts the sender grants nothing. Revoking a peer disables future imports immediately. Cached bytes already disclosed cannot be recalled; retained resources remain provenance-labelled and are no longer eligible for new context use without a fresh explicit local approval. Re-pairing never resets replay receipts. Expiry is an admission rule, not a promise of deletion or cryptographic recall.

### Resource envelope

The JSON carrier is strict: reject unknown or duplicate fields, invalid UTF-8, trailing data, and values outside the following profile. Hex is lowercase, even-length and fixed-length where stated. Integers are nonnegative JSON integers, not floats. Every external file is opened using no-follow and nonblocking flags where supported, checked via the opened descriptor to be regular, and read with a hard byte limit before parsing. A plain blocking open followed by metadata is insufficient: a FIFO can block before inspection. Platforms lacking equivalent safe primitives must reject unsupported input, not silently weaken this boundary.

```json
{
  "protocol": "control-federation/0.1",
  "sender": "ed25519:<64 hex>",
  "recipient": "ed25519:<64 hex>",
  "kind": "lens_bundle",
  "sequence": 1,
  "resource_id": "sha256:<64 hex>",
  "issued_at": 1793145600,
  "expires_at": 1793232000,
  "payload": "<exact UTF-8 JSON payload string>",
  "signature": "<128 hex>"
}
```

`resource_id` is SHA-256 of the exact decoded payload bytes. Payload encoding is opaque for signature verification, avoiding a home-grown canonical JSON algorithm. Sender implementations serialize the payload once and sign those same bytes. Independently serialized semantic equivalents MAY have different resource IDs. Byte identity is required; global semantic identity is not.

The signature uses Ed25519 over a domain-separated, unambiguous preimage. Start with the UTF-8 bytes `control-federation/0.1` and a NUL byte, then encode sender, recipient, kind, sequence, resource_id, issued_at, expires_at, and payload in that exact order. Each field is encoded as its unsigned 64-bit big-endian byte length followed by its exact UTF-8 bytes. Integers use canonical decimal ASCII without leading zeros. The verifier reconstructs exactly this preimage; signatures bind recipient, kind, sequence, digest and validity. There is no unsigned endpoint or label used for authority. The outer JSON carrier's escaping/whitespace is not signed; the decoded payload string bytes are.

Initially only `lens_bundle` is accepted. The typed payload is `{ "roots": ["engineering"], "definitions": [<existing Definition schema>], "documents": [<existing DocumentInput schema>] }`. It contains exactly one selected revision per definition/document ID, the exact transitive closure of included definitions and documents, and no unrelated resources. Resolve only against this bundle's inputs, never destination-local current revisions. Missing references, duplicate IDs (even differing revisions), cycles, and an inconsistent resolved result are rejected. Bare local include IDs are safe only because the bundled graph is closed and unique.

By default exports containing any referenced document fail with a clear request for an explicit `--include-documents` decision. This avoids publishing document bodies merely because the operator named a lens. With that flag, bodies are exact selected snapshots; receiver trust must separately permit document inclusion. Export reports the transitive selected document titles and serialized size; providing the explicit flag authorizes writing that selected bundle, without an additional interactive confirmation requirement. A definition-only lens with no document references needs no flag. A future `document_bundle` may share documents independently; it is not accepted in 0.1.

Tool IDs are inert references checked against the known descriptor schema; a remote resource cannot declare executable tool implementations. The first version rejects unknown descriptor IDs, matching the existing pure resolver. A future generic inspector could retain unknown IDs as inert data, but cannot activate them without a supported locally authorized implementation. Privacy review is the operator's responsibility; signatures do not encrypt content.

Recommended fixed initial limits: carrier 2 MiB, decoded payload 512 KiB, 128 definitions, 128 documents, 128 roots, 256 tool IDs, 64 KiB per text field, lifetime at most seven days, 256 imported objects and 64 MiB total persisted import bytes per local store. Both object count and aggregate stored bytes are bounded, including receipts/provenance rather than only document lengths. Implementations MUST publish their actual stricter limits. Reject at the boundary; do not truncate signed content or automatically evict replay records to admit more input.

### Acceptance transaction and replay

Import checks, in order: bounded regular file; strict carrier profile; recipient equals local node; pinned non-revoked sender and allowed kind; timestamps not expired and no more than five minutes in the future; bounded lifetime; valid digest and signature; bounded valid payload. Invalid data produces no accepted-object state.

The replay receipt key is `(sender, recipient, sequence)`; the accepted resource identity includes its sender and resource_id. Export allocates a persisted, monotonically increasing sequence and never reuses one after a crash; gaps are harmless. First import atomically persists immutable payload, signature/provenance, and receipt metadata. An exact repeat is an idempotent duplicate success without creating another object or triggering a side effect. Reusing a sequence for different signed bytes is rejected as a conflict. Out-of-order unused sequences MAY be accepted so file delivery order does not become a hidden availability requirement. Durable receipts remain after revocation/re-pairing; hitting capacity rejects new imports rather than forgetting replay history. Acceptance MUST be serialized against revocation and concurrent imports. A failed persistence step does not acknowledge success. A single local state lock or equivalent atomic serialized transaction is required.

Imported resources live in a separate federation store and are inspectable by operator CLI. Local lens/document state is unchanged. Using one in a chat requires a later explicit activation/import-to-local operation with a new local ID, source attribution, and local policy checks. That operation is outside the initial slice. Re-export never asserts that the original author signed a modified payload.

CLI shape (names may follow existing command conventions): `control federation identity`, `trust <node-id> --label <label> --root <lens-id>`, `revoke <node-id>`, `export <lens-id> --recipient <node-id> --output <file>`, `import <file>`, and `status`. Private key material is never printed. Identity prints only public identity. Re-pairing a revoked identity must be an explicit operator action, not a side effect of import.

## Future transport: directed, authenticated inboxes

A future dedicated federation listener is separate from browser/operator endpoints. It accepts only versioned, bounded protocol messages, with per-peer request/concurrency/storage limits and no automatic fetch of arbitrary URLs from bodies. Ingress is authenticated by pinned node signatures, with TLS for confidentiality and server authentication. Tailscale is useful transport policy, not application authorization.

Network messages add a recipient-bound request ID, short expiry, and operation-specific body digest. The durable inbox records the idempotency outcome before acknowledgement; outboxes retry with bounded exponential backoff and jitter. Servers never claim exactly-once execution across failures. Request retries return the recorded outcome, not repeated authority checks that launch another run. Unknown peers cannot create an inbox or consume unbounded resources. Never accept unsigned redirects, claimed forwarded operator identity, or transitive grants.

No shared global order is required. Each node is authoritative for its local policy and task state. Resource versions are immutable; deliberate local merge produces a new version. Initial replication is selective pull/directed delivery, not a CRDT for every chat and not consensus across the collective.

## Future delegated jobs and deterministic reconciliation

Remote execution is blocked until hard per-attempt writable-volume quotas, bounded artifact export, authenticated ingress, durable admission limits, cancellation, and startup worker reconciliation have been implemented and tested. The current CPU/memory/pid limits do not enforce disk quotas. Polling directory sizes is not a hard storage bound. Marking a run interrupted is not orphan cleanup.

A future job offer names: protocol/request ID; issuer/recipient node IDs; bounded origin principal reference; explicit task class (`task`, never implicit `improve`); immutable context resource IDs; exact persona/policy revisions; pinned worker image digest; selected capabilities; deadline; max attempts; max child count/depth; CPU/memory/pids/writable bytes/artifact bytes; model/token limits; and output destinations. The local receiving operator authorizes the intersection; unknown identity or capability fails closed. Credentials remain receiving-node-owned. No Docker socket, arbitrary host mount, operator cookie, or automatically executable remote Nix expression is accepted.

Job state is receiver-authoritative: `offered -> accepted -> queued -> starting -> running -> succeeded|failed|cancelled|expired|lost`. Policy can transition `offered -> rejected`. Admission atomically reserves local budgets before `accepted`. Attempts have monotonically increasing fencing generations. A completion must match the active generation; stale completion is recorded as evidence and cannot change the job outcome. Terminal state never returns to running; explicit retry creates a new attempt within the reserved retry budget. Cancellation persists intent first and becomes terminal only after confirmed process termination. If termination is uncertain, retain resources/mark lost and block conflicting retry rather than asserting safe cancellation.

Before launching, the supervisor checkpoints attempt identity and desired state, then creates a runtime-labelled container `(node, job, attempt, generation)` with quota-backed volumes and a hard deadline. Startup reconciliation enumerates only this node's owned labels, compares durable state, terminates or safely adopts known workers, and prevents new dispatch until discrepancies are resolved. A single active node-state writer lock is required. Containers must have a runtime-enforced lifetime independent of the web process, or an equally reliable external supervisor; an in-process timer alone is insufficient.

Partitions cannot manufacture certainty. The sender records remote outcome as unknown after a lease/deadline uncertainty and must query/cancel/reconcile before retrying elsewhere. The receiver never extends authority merely because the sender is unavailable. Token quotas require enforcement at a provider broker or trusted harness, not only after-the-fact accounting. Unsupported hard limits reject the job rather than pretending the declared limit was enforced.

Agents may propose or edit declarative workflow DAGs, choose peers, ask for more tools, and judge semantic evidence within explicit delegated management scopes. The deterministic supervisor validates acyclicity, budgets, authority, dependencies, state transitions, and restart policy. Bounded `while`/event subscriptions require explicit stop conditions, event deduplication, deadlines and iteration budgets. Scheduling an eligible queue item, retry backoff, and process cleanup never require a model call. Judges can recommend acceptance; reproducible checks remain distinct from model agreement. No general workflow programming language or Kubernetes-sized controller framework is necessary for this first state machine.

## Threat model and verification

Treat peers, agents, imported instructions, resource titles, and documents as potentially hostile. Protect against identity spoofing, recipient substitution, payload/header tampering, replay, duplicate execution, path traversal, symlink/FIFO reads, oversized input, unsigned field confusion, privilege confusion, forged provenance, dependency cycles, stale completion, malicious peer floods, and accidental private-context publication. Display imported text escaped/sanitized in browser/TUI. A signature is authenticity, not fact verification. A peer can retain anything deliberately shared with it.

First-slice acceptance tests use two fresh temporary node directories and no credentials/network/model calls: identity persists across restart; independent identities differ; explicit trust required; valid bounded snapshot is accepted once; duplicate creates no second record; wrong recipient, wrong sender, changed body/header/signature, expired/future/overlong validity, unknown kind/fields, bad lengths/hex, invalid schema and oversized regular file are rejected; symlink/FIFO rejected without blocking; revocation blocks subsequent imports; concurrent/restarted imports remain idempotent; persistence failures never acknowledge success; import does not modify local lenses, chats, roles, run queue, or tool registry. Existing operator/browser protection and worker credential boundaries must remain intact.

Network readiness adds TLS/authentication interoperability, bounded slow-client and concurrent-input tests, rejection of forged proxy identity, crash-before/after-ack tests, idempotent cancellation, hard disk-limit experiments, orchestrator-killed orphan reconciliation, stale-generation completions, and partition/retry scenarios. A successful two-file exchange is not evidence for these future guarantees.

## Implemented profile and use

`control federation help` lists the commands. State lives in `.control/federation`; `CONTROL_FEDERATION_DIR` can select an independent node directory. The CLI uses an exclusive OS file lock, private Ed25519 PKCS#8 identity, and a single atomic ledger containing trust scopes, export sequence, envelopes and receipts. Imports never write `.control/state.json`. `inspect INDEX` shows the original payload and provenance; `status` lists pinned/revoked peers and quarantine indexes. These are local operator commands, not MCP capabilities exposed to agents.

Actual limits: 2 MiB carrier, 512 KiB decoded payload, 128 roots/definitions/documents, 256 total tool references, 128 peers, 256 retained delivery receipts, and a 64 MiB serialized ledger. Root scopes are explicit IDs; wildcards are unsupported. Existing resolver limits also apply: valid IDs up to 64 ASCII characters, names up to 80 characters, instructions up to 4,000 characters per definition, nesting below 32, and 48,000 aggregate context characters. Document title/body each have a 64 KiB byte limit. Generated exports expire after one day; receivers accept at most seven days and five minutes of future clock skew. File IO supports macOS and Linux; other platforms fail closed. Parent directories of configured paths must be operator-controlled; no-follow protects the final file component, not arbitrary hostile ancestor replacement.

Example, with two node directories on this laptop (replace public ID placeholders with `identity` output):

```sh
CONTROL_FEDERATION_DIR=/private/tmp/control-a control federation identity
CONTROL_FEDERATION_DIR=/private/tmp/control-b control federation identity
CONTROL_FEDERATION_DIR=/private/tmp/control-b control federation trust NODE_A --label laptop --root engineering
CONTROL_FEDERATION_DIR=/private/tmp/control-a control federation export engineering --recipient NODE_B --output /private/tmp/engineering.control.json
CONTROL_FEDERATION_DIR=/private/tmp/control-b control federation import /private/tmp/engineering.control.json
CONTROL_FEDERATION_DIR=/private/tmp/control-b control federation inspect 0
CONTROL_FEDERATION_DIR=/private/tmp/control-b control federation revoke NODE_A
```

Run export from the source node's working directory to select its current local library. Explicit `--include-documents` is required when that selection contains document bodies; receiver trust separately needs `--allow-documents`. The output path must not exist, and the file is private. No pairing on the exporting side is required to send to an explicitly supplied recipient public key; only inbound acceptance grants are stored.

A sequence is reserved durably before export. Failed output creation can leave a sequence gap. Ledger writes use create-new temporary files, file sync, atomic rename, and directory sync. If rename succeeds but directory sync fails, the result is uncertain: the command reports failure without acknowledgement, and reopening/retrying discovers any committed receipt. Never restore an old ledger backup while retaining a node identity: that could reuse export sequences and lose replay records. Recover with a new identity unless replay/sequence continuity is independently guaranteed. Capacity exhaustion rejects input rather than evicting receipts.

Future delivery, delegated jobs, imported-resource activation, key rotation, and a generic workflow interpreter are specified directions, not implemented features of this profile.
