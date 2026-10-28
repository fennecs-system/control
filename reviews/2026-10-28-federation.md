# Federation protocol adversarial review

This is a design critique by a separate agent, not reinforcement learning or a security certification. No credentials, external chats, or live model jobs were used.

## Initial recommendation

Use an offline, bounded, authenticated resource exchange as the first implemented slice. Explicitly call it an offline federation foundation, not remote agent execution or a working internet federation. The current writable worker bind has no hard disk quota and worker crashes lack reconciliation; expanding remote execution before those gaps are closed would enlarge an existing denial-of-service boundary.

ActivityPub supplies useful directed delivery and actor/inbox/outbox vocabulary. AT Protocol supplies stable cryptographic identity, immutable content identity, and separation of storage from views. Neither social protocol is an execution-authority scheme. Control should borrow these properties rather than add public discovery, relays, global feeds, or claim wire compatibility.

Primary sources reviewed:
- https://www.w3.org/TR/activitypub/
- https://atproto.com/specs/atp
- https://atproto.com/specs/repository

## Concrete author exchange

1. Author proposed signed immutable lens/document exchange, pinned directional peers, quarantine, and deferred networking/execution. Critic agreed this is a useful first slice provided users can inspect received data and the implementation does not claim transport it lacks.
2. Critic found a dependency-pinning trap in the existing lens resolver: bare child ids resolve to the maximum local revision. A signed export must contain exactly one chosen definition per id, explicit roots, and exact document snapshots. Resolution must use only that closed bundle, never destination records. Otherwise two nodes can interpret the same purported lens snapshot differently.
3. Critic recommended signing domain-separated exact UTF-8 payload bytes rather than inventing canonical JSON. Outer JSON escaping is transport, not the signed representation. Typed, strict parsing follows signature verification. Equivalent JSON with different whitespace may have different content ids; the protocol must say so.

## Required invariants

- The local operator pins a cryptographic public key. Display names and sender-declared roles grant no authority.
- Every accepted delivery binds protocol version, sender, recipient, kind, resource id, issue/expiry interval, and exact payload bytes. Domain separation and unambiguous length framing are specified.
- Unknown or revoked peers fail closed, including before returning success for a previously accepted receipt. Re-adding a peer does not erase replay history.
- Trust is directional. The receiver's allowlist limits resource ids and kinds. Trusting one lens root cannot implicitly authorize export of arbitrary private documents; export content must be visible and individually authorized by its source operator.
- Imports cannot overwrite or activate local definitions, tools, documents, roles, sessions, or policy. Received lens instructions remain untrusted content until an explicit later local action.
- Bundle-local dependency resolution is closed and unambiguous. Reject missing references, cycles, duplicate definition/document identities, incompatible tools, mismatched root ids, and resource-kind mismatches.
- Body length, graph depth/cardinality, UTF-8 size, import-file size, receipt count, and total quarantine storage have hard bounds before allocation or mutation. Do not accept symlinks or arbitrary host-path destinations for imported artifacts.
- Expiry limits delivery acceptance, not the truth or deletion of imported content. Revocation blocks future deliveries; it does not retract copies already received.
- Import and receipt commit atomically. Retries after a durable success are idempotent. Failed saves cannot acknowledge acceptance or leave half-imported trusted records.
- No secrets, private chat history, operator cookies, provider credentials, or execution authority are exported with a lens.

## Deterministic control plane

Agents may propose declarative desired state and bounded scheduling decisions. A deterministic controller validates policy, grants, quotas, idempotency, leases, fencing generations, retries, and observed worker state. An LLM must not be invoked merely to poll whether a process is alive or decide an expired lease; it cannot manufacture authority by writing a declaration.

Future remote jobs require recipient-side authorization at admission and dispatch; explicit source identity must remain distinct from a local architect. At-least-once delivery needs idempotency, not exactly-once claims. Leases limit coordination, not physical worker lifetime: a disconnected worker can keep running, so stale results need fencing and cleanup requires a local supervisor. Remote content acceptance never automatically starts work.

## Acceptance cases to require

- Two distinct local node identities exchange one lens snapshot successfully without credentials or network exposure.
- Tampering with payload, recipient, sender, kind, resource id, expiry, or signature fails.
- Wrong recipient, unknown/revoked sender, unsupported version, excessive interval, and expired delivery fail.
- Root-only scopes cannot sneak in unsanctioned document content; malformed or ambiguous graphs fail before mutation.
- Repeat delivery is idempotent; re-pairing does not erase history; revoked repeat delivery remains denied.
- Failed persistence leaves prior state intact. Symlink and oversized import files fail safely.
- Received records remain separate from local active records; private chats and policy are unchanged.

## Remaining review

Review the author's concrete draft and implementation claims before considering the spec converged. Offline exchange cannot establish live job isolation, tailnet TLS configuration, scheduler reliability, hard worker storage bounds, or orphan cleanup.

## Draft review and requested revision

Read the complete first draft. Its future job section correctly distinguishes leases from physical process lifetime, records uncertain partition outcomes honestly, requires hard writable-volume limits, fences stale completions, and makes supervision deterministic rather than token-consuming.

Requested five substantive corrections directly from the author:

1. Replace the phrase "existing resolved-context schema" with a closed bundle carrying full definitions. The existing `Resolved` struct retains only `LensVersion` summaries, so a recipient could not recompute a graph or validate cycles from it.
2. Add root-resource scopes rather than a kind-only peer allowlist. Require explicit document disclosure on export and explicit document acceptance on import. Default to no document bodies.
3. Open untrusted files with nonblocking/no-follow flags before checking regular-file metadata, otherwise a FIFO can block at open before the promised validation occurs.
4. Retain replay state across peer re-pairing and cap total stored bytes, not only object count.
5. Align the wire profile with the implementer's single exact UTF-8 payload string and length-prefixed domain-separated signature inputs, rather than maintain an incompatible hex/newline variant.

These requests were sent in a concrete author/critic revision exchange. Final convergence still requires checking the revised draft.

## Convergence

Reviewed specification 0.1, review revision 1. The author incorporated the five requested corrections: closed unique DAGs, directional root scopes and document opt-in, safe nonblocking descriptor-first file validation, replay retention and aggregate storage limits, and exact-string length-prefixed signatures. The implementation should reject unknown tool descriptor IDs consistently with the current resolver rather than imply inert unknown descriptors are already inspectable.

The resulting minimal slice is acceptable as an offline federation foundation. It remains explicitly separate from remote execution. The parent should compare implemented limits and test results to the normative text before committing. No independent security claim is made for code that had not yet been written during this protocol review.

## Implementation review

Read the first complete `src/federation.rs` implementation and the subsequently appended tests, without opening runtime state or credentials. No additional crypto or authority bypass was found in this source review. Scope checks, recipient-bound signatures, typed closed-graph validation, quarantine separation, bounded IO, persisted sequence allocation, and revocation-before-duplicate checks match the agreed design.

Reported and corrected by the parent: the shared nested `DocumentInput` type initially lacked `deny_unknown_fields`, allowing signed nested document extras despite a strict payload promise. The parent added the restriction. Requested a regression case covering this nested position.

Reported durability wording: rename may commit a ledger before directory fsync fails. The command must not acknowledge success on that error; a later retry can discover an already committed idempotent receipt. It cannot promise that every persistence error leaves the old on-disk state untouched.

Minor reliability observation: failures after creating a unique temporary file can leave that file behind and block a later write using the same pid/sequence. Cleanup may remove only a temporary file created by the current command, never a preexisting planted file. This is not an offline peer authority escalation.

The source review does not establish passing tests or live federation readiness. Parent is running compilation and tests separately.

## Implementation verification by parent

The strict nested-document regression is covered. Temporary write cleanup now removes only files successfully created by that command; preexisting paths remain untouched. Directory-sync uncertainty is documented in the spec.

Validation: 73 workspace tests passed (the default suite excludes live integration tests); Clippy with warnings denied passed; release binaries built. A separate isolated-server TUI integration test passed after adding keyboard lens selection/application. A two-node CLI smoke test accepted an engineering bundle, recognized a duplicate, and denied the same delivery after revocation. No model calls, network peer listener, remote jobs, or production chat mutations were used in these checks. The rebuilt TUI also connected to the running local service and its lens menu was exercised interactively without applying changes.
