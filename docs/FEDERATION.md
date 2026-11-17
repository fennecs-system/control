# Federation

Federation lets independently operated Control nodes share selected resources
without making a peer an operator. Today it is an offline, operator-mediated
exchange of signed lens bundles (`src/federation.rs`). There is no network
listener, no live delivery and no remote execution. The design was reviewed in
[the federation review log](../reviews/2026-10-28-federation.md).

## Foundations

Control borrows ideas, not wire formats:

- [ActivityPub](https://www.w3.org/TR/activitypub/#server-to-server-interactions):
  directed delivery, explicit recipients, stable identifiers and deduplication.
- AT Protocol ([identity](https://atproto.com/guides/identity),
  [repositories](https://atproto.com/guides/data-repos),
  [stack](https://atproto.com/guides/the-at-stack)): durable identity separate
  from names and location, and immutable content-addressed data.

Control does not implement ActivityPub, DIDs, AT repositories, relays, public
discovery, consensus or a global ledger. A public firehose is a poor default for
private source code and chats.

## Invariants

1. Every node is independently administered. Joining never makes a peer or its
   agent a local operator.
2. An authenticated identity proves who sent bytes, not that they are true, safe
   or authorized.
3. Effective capability is the intersection of issuer delegation, peer scope,
   local policy, resource policy and run constraints. Lenses grant no capabilities.
4. Private chats, source trees, credentials, operator cookies and role bindings
   are never replicated implicitly.
5. An imported object is an immutable foreign resource. It cannot overwrite a
   local lens or document, activate a tool, install UI, start a run or execute Nix.
6. State transitions, authorization, retries, deadlines, quotas and cleanup are
   deterministic code. Agents may propose plans; they cannot bypass these checks.
7. Federation works without an always-running model.

## Signed lens bundles

### Identity and trust

Each node has an Ed25519 key (PKCS#8, generated from OS randomness) stored in its
private federation directory, never in the repository or worker mounts. The node
ID is `ed25519:` followed by 64 lowercase hex characters of the public key.
Changing the key creates a new identity; there is no key rotation.

`trust` pins an exact peer ID with a label, one or more explicit root lens IDs
and an optional `--allow-documents` permission (off by default). Wildcard roots
are not supported. Self-trust is rejected. Trust is neither reciprocal nor
transitive. `revoke` blocks future imports immediately; bytes already received
cannot be recalled. Re-trusting a revoked peer is an explicit `trust` command and
never resets replay receipts. Expiry is an admission rule, not deletion.

### Envelope

The carrier is strict JSON: unknown fields are rejected and hex must be lowercase
with the stated length.

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

`resource_id` is the SHA-256 of the exact payload bytes. The payload is signed as
opaque bytes, so no canonical JSON is needed; semantically equal payloads may have
different IDs.

The Ed25519 signature covers a domain-separated preimage: `control-federation/0.1`
and a NUL byte, then sender, recipient, kind, sequence, resource_id, issued_at,
expires_at and payload in that order. Each field is its unsigned 64-bit
big-endian length followed by its UTF-8 bytes; integers are decimal ASCII. The
outer JSON escaping is not signed.

Only `lens_bundle` is accepted. Its payload is
`{ "roots": [...], "definitions": [<Definition>], "documents": [<DocumentInput>] }`
and must contain exactly the transitive closure of the roots: one revision per
ID, no duplicates, no cycles, no missing references and nothing unrelated. It
resolves only against its own contents, never the receiver's lenses.

Exporting a lens that references documents fails unless `--include-documents` is
given; the receiver must also have granted `--allow-documents`. Export prints the
included document titles and the byte size. Tool IDs are inert references; a
bundle cannot declare tool implementations, and unknown IDs are rejected by the
existing resolver. Signatures do not encrypt content; privacy review is the
operator's responsibility.

### Import

Import checks, in order: a bounded regular file (opened with `O_NOFOLLOW |
O_NONBLOCK`, so symlinks and FIFOs are rejected without blocking); the strict
carrier; protocol, kind, nonzero sequence and recipient equal to this node; a
pinned, non-revoked sender; validity not expired, at most five minutes in the
future and at most seven days long; payload digest; signature; payload schema and
limits; and the peer's root and document scope. Invalid input creates no state.

The replay key is `(sender, sequence)` on this node. An exact repeat is an
idempotent duplicate with no new record. A different envelope with a used
sequence is rejected as a conflict. Sequences may arrive out of order. Export
reserves its sequence durably first, so a failed export can leave a harmless gap.
Receipts survive revocation and re-trust. At capacity, imports are rejected;
receipts are never evicted.

Accepted bundles stay in quarantine in the federation ledger. They never change
the node's lenses, documents, chats, roles, run queue or tool registry. There is
no activation step yet.

### Storage and durability

The federation directory is `federation/` under the node state directory
(`~/.local/state/control/federation` by default), or `CONTROL_FEDERATION_DIR`.
Each command holds an exclusive file lock. Files must be private (`0600`). The
ledger (`ledger.json`) holds trust scopes, the export sequence and imported
envelopes. Writes use a create-new temporary file, file sync, atomic rename and
directory sync. If the directory sync fails after a rename, the command reports
failure; reopening finds any committed state.

Never restore an old ledger while keeping the node identity: that could reuse
export sequences and lose replay records. Recover with a new identity instead.
`O_NOFOLLOW` protects only the final path component, so parent directories must
be operator-controlled. File I/O is supported on macOS and Linux; other platforms
fail closed.

### Limits

| Limit | Value |
| --- | --- |
| Carrier file | 2 MiB |
| Decoded payload | 512 KiB |
| Roots, definitions, documents | 128 each |
| Tool references | 256 in total |
| Document title, body | 64 KiB each |
| Peers | 128 |
| Retained imports (receipts) | 256 |
| Serialized ledger | 64 MiB |
| Export lifetime | 1 day (receivers accept up to 7 days) |
| Future clock skew | 5 minutes |

The lens resolver's own limits also apply (ID, name, instruction, nesting and
aggregate context sizes; see `crates/context`).

## Commands

`control federation help` lists the commands. They are local operator commands,
not tools exposed to agents. Private keys are never printed.

| Command | Effect |
| --- | --- |
| `identity` | Print this node's public ID |
| `trust NODE --label NAME --root LENS [--root LENS] [--allow-documents]` | Pin a peer and its inbound scope |
| `revoke NODE` | Block future imports from a peer |
| `export LENS --recipient NODE --output FILE [--include-documents]` | Write a signed bundle of a local lens |
| `import FILE` | Verify and quarantine a bundle |
| `inspect INDEX` | Show a quarantined payload and its provenance |
| `status` | List peers and quarantined imports |

Export reads the lens library from the node's state, writes a new private file
(the output path must not exist) and needs no trust entry for the recipient.

Example with two federation directories on one machine (replace `NODE_A` and
`NODE_B` with the `identity` output):

```sh
CONTROL_FEDERATION_DIR=/private/tmp/control-a control federation identity
CONTROL_FEDERATION_DIR=/private/tmp/control-b control federation identity
CONTROL_FEDERATION_DIR=/private/tmp/control-b control federation trust NODE_A --label laptop --root engineering
CONTROL_FEDERATION_DIR=/private/tmp/control-a control federation export engineering --recipient NODE_B --output /private/tmp/engineering.control.json
CONTROL_FEDERATION_DIR=/private/tmp/control-b control federation import /private/tmp/engineering.control.json
CONTROL_FEDERATION_DIR=/private/tmp/control-b control federation inspect 0
CONTROL_FEDERATION_DIR=/private/tmp/control-b control federation revoke NODE_A
```

## Threat model

Treat peers, agents, imported instructions, titles and documents as potentially
hostile. The exchange defends against identity spoofing, recipient substitution,
tampering, replay, path traversal, symlink and FIFO reads, oversized input,
unsigned-field confusion, forged provenance, dependency cycles and accidental
publication of document bodies. Displayed payload text has control characters
removed. A signature shows authenticity, not truth. A peer can keep anything
deliberately shared with it.

The tests in `src/federation/tests.rs` use temporary node directories with no
credentials, network or model calls.

## Not implemented

These directions are specified for future work; none exist today.

**Network delivery.** A separate federation listener, not the operator HTTP
interface, accepting only versioned, bounded, signed messages over TLS with
per-peer limits. Messages carry a recipient-bound request ID, short expiry and
body digest. A durable inbox records the outcome before acknowledging; outboxes
retry with bounded backoff and jitter. No exactly-once claims, unsigned
redirects, forwarded operator identity or transitive grants. No global order:
each node is authoritative for its own policy and state, and replication is
selective, not a CRDT or consensus. Tailscale is transport, not authorization.

**Delegated jobs.** Blocked until hard per-attempt disk quotas, bounded artifact
export, authenticated ingress, durable admission limits, cancellation and startup
worker reconciliation are implemented and tested. Current CPU, memory and PID
limits do not bound disk. A job offer would name the task class, immutable
context IDs, pinned persona/policy revisions and worker image digest,
capabilities, deadlines, attempt/child limits, resource and token limits and
output destinations; the receiving operator authorizes the intersection and
unknown identities or capabilities fail closed. Job state is receiver-owned
(`offered → accepted → queued → starting → running → succeeded | failed |
cancelled | expired | lost`, or `offered → rejected`), with fenced attempt
generations so stale completions cannot change the outcome. Cancellation is
terminal only after confirmed termination. Partitions leave outcomes unknown
until reconciled. Token limits need enforcement at a provider broker, and
unsupported hard limits reject the job.

**Other gaps.** Activating an imported resource as a new local lens with
attribution, a `document_bundle` kind, key rotation and endpoint discovery.
