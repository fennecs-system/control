# State and effect boundaries

Application operations borrow the domain state they need, apply a typed input
and return ordered effect descriptions. A driver interprets the effects and
feeds confirmed results back. Rust needs no monad framework for this, and
operations never get ambient access to the whole application `State`.

```mermaid
flowchart TD
    Clients["Web / TUI / MCP / Discord adapters"] --> API["Typed application command and result"]
    API --> Driver["Application driver: authority and exclusive ownership"]
    Driver --> Transition["Borrowed domain views → transition"]
    Transition --> Effects["Typed ordered effects"]
    Effects --> IO["Journal / checkpoint / dispatcher interpreter"]
    IO --> Results["Confirmed completion or failure"]
    Results --> Transition
```

## Operations on this boundary

**Directed agent messages** (`src/application/messages.rs`). Planning reads
borrowed run, conversation, service, receipt and bot views with an explicit run
ID; it uses no global `State`, JSON, clock, filesystem, lock, channel or
executor. Planning does not mutate state. Applying a plan changes only the
recipient's messages and revision, the run ledger and notice, and the bounded
receipt cache, then returns ordered effects:

1. Archive completed peer evidence (descendant follow-ups skip this).
2. Checkpoint the cross-domain update.
3. Notify the dispatcher, only after the checkpoint is confirmed.

A repeated request returns its stored receipt without effects. The host rechecks
source and recipient authority before applying. Lineage, service generation,
stop scope and capacity rules still apply, and the bot supervisor still decides
whether a message can admit an unattended turn. The driver in
`src/agent_message.rs` holds the application mutex through authorization,
planning, application and persistence. Its transaction guard restores only the
touched fields on failure or unwind before a confirmed checkpoint, and keeps the
committed receipt after confirmation.

**Room wakes** (`src/application/room_wakes.rs`). Planning consumes borrowed
runs, conversations, posts and subscriptions, with an authorization predicate
supplied by the host, and returns wait/advance/disarm/queue decisions. Some
legacy records still carry journal-backed fields, so this is not yet a fully
I/O-free projection. Plans hold positional indices and must be applied under the
same exclusive ownership, with no other writer or suspension in between.

**Room notifications** (`src/application/room_notifications.rs`). Advance,
disarm and queue changes apply through narrow run, chat and wake fields and
return a checkpoint effect plus, for queued notifications, a dispatcher wake.
The blocking interpreter in `src/rooms/notifications/driver.rs` holds the state
borrow through persistence and uses the same guard rules. Cursor-only changes
checkpoint without waking a worker.

**Continuations** (`Submission::into_continuation` in
`src/application/runs.rs`). A continuation borrows the origin's captured
settings and builds a fresh run through `into_run`. Captured models (including
none), lenses and lineage carry over; attempts and stale grants do not. The host
keeps actor authorization, capacity, current grants, clock and IDs, queue order
and dispatcher notification.

**Approval resumption** (`src/application/approval_resume.rs`). A read-only
check over borrowed runs returns `Wait`, `Note` or `Candidate(index)`.
Publication takes an `approval_resume::Edit` holding only runs, the queue notice
and borrowed chat records; `chat::continuation` prepares the admitted run
without changing state. Port and bot drivers acknowledge their request records
in the same checkpoint and wake the dispatcher only after it succeeds. A
rejected save restores the prior edits and sends no wake; reload does not replay
acknowledged requests. Request grouping and rollback still live in those
drivers.

**Worker tool dispatch** (`src/worker_tools/`). `authority::admit` is the one
check that the caller's run is running, authorized and on its current attempt.
It returns the `Caller` token that handlers take instead of finding the run
again. Each tool name has a table entry (`table.rs`): the run-level fences it
passes and a handler whose kind (`kinds.rs`) fixes how it uses the state lock.
`Pure` never sees state. `Read` builds its reply from an owned copy after
release. `Write` mutates under the lock and checkpoints through the save the
dispatcher passes. `External` and `Family` handlers work only after release.
The brokers behind the table (rooms, spawning, delegation, design, knowledge and
the attempt families) still re-check the run and roll back their own saves.
`src/worker_tools/tests/characterisation.rs` pins every tool's answer.

**State handle** (`src/store.rs`). Every lock of `State` goes through
`Store`. `read` and `write` run a closure under the lock; its result cannot
borrow the state, and `read` cannot change it. Each returns `Unavailable` when
a panic poisoned the lock, and the caller keeps its own answer (503, an error,
a skipped tick). `guard` holds the lock across statements where a closure
cannot yet express the critical section: worker tool dispatch moves it into
the handler, and a few drivers release it mid-branch or inside a loop. Only
`store.rs` names the mutex (`tests/store_boundary.rs`), so a single writer or
published snapshots would change that module alone.

## Alternate driver conformance

The messages and room-notification operations also run under a test-only Tokio
driver (`tokio` is a dev-dependency only) with fake archive and checkpoint
effects. Conformance tests cover ordering, duplicate and restart receipts,
failed archive or checkpoint rollback, unwind, dropping a suspended transaction
before and after a confirmed commit, waiting writers and stale source attempts.
This shows the transitions do not depend on a concurrency runtime. It is not a
production async persistence adapter, a speed comparison or a test of
cancelling in-flight filesystem I/O.

## Checkpoint durability

`storage::checkpoint` distinguishes failures before replacement
(`CheckpointError::NotReplaced`) from failures at rename or directory sync
(`CheckpointError::Unconfirmed`). Success requires both file and directory sync
before state-change subscribers are notified. On an unconfirmed write the
adapter, still holding exclusive ownership, compares the published file with the
intended bytes, then syncs file and directory. Only confirmed equality and
durability count as success; a visible file alone does not.

`storage::save`, used by all production checkpoint callers, returns ordinary
errors only before replacement. If reconciliation cannot settle the outcome, it
requests node shutdown and unwinds, poisoning the state mutex, so no request or
shutdown path can save an in-memory rollback over uncertain disk state. Startup
confirms the checkpoint file and directory before journal recovery and dispatch.
Fault tests inject directory-sync failure and byte mismatches, and check
shutdown, 503 and poison fencing in a child process. They are filesystem fault
injections, not power-loss simulations.

One `control_journal::writer::Writer` per state directory owns `state.json`
and `state.log`. A save encodes and diffs under the state mutex, advances the
diff base, queues the record and gets a ticket. Waiting on it, a caller that
finds nobody writing becomes the leader and writes everything queued so far on
its own thread, with one write and one sync (a compaction contains the lines
queued before it); callers arriving meanwhile are written by the next leader.
There is no writer thread. The bytes on disk are those
the synchronous path wrote (`storage::tests::log` pins them). `storage::save`
waits for its ticket under the mutex, so its callers see no change.
`storage::submit` returns before durability; only the store's driver
(`Store::commit`) calls it. The driver locks, refuses at once while storage
is unavailable, takes the clock reading (`Tx`), applies the operation, stages
the save, unlocks, waits, and only then releases the operation's effects
(`application::effects::Effects`, today the dispatcher wake of a queued run)
with the writer's `Durable` proof. A save refused while the lock is still
held is undone by the operation's own undo. Converted so far: `POST
/dna/save`, `/task`, `/retry-run`, `/room`, `/spawn` and chat send (`POST
/dm/{id}/messages`). Runs enter the queue only through `runs::admit` (pure,
clock and ids from `Tx`) and `Runs::queue`, whose `Queued` must become an
effect; legacy drivers that still save under the lock release it with
`Effects::after_held_save`, so no wake precedes a save.

A change the driver released is visible before it is durable. The store
keeps the newest such change's mark; `read_response` and `state_response`
take it with their view (`Store::read_seen`, `write_seen`) and write their
reply only once it is durable or storage is unavailable (then the change
stays queued). With nothing in flight this costs an uncontended lock and one atomic load. Other routes
(MCP tools that change state, bots, ports, services pages) are not gated
yet; the read-only operator MCP tools are (`session_http::reads`).

A failed save is an error, not a reason to stop. The policy is one function
(`writer::retry_then_degrade`): a write that leaves the files intact
(`NotReplaced`: a full disk, an I/O error on append) is retried twice within
about 1.5 seconds, then the writer is degraded (`writer::Health::Degraded`,
read with `storage::health`). While degraded, new saves are refused with
"storage unavailable: <cause>; not saved": `storage::save` refuses before its
archive step, but its callers have already changed memory and roll back as
for any `NotReplaced`; `Store::commit` checks before applying the change.
Reads go on. A save whose caller still waits under the mutex is refused and
rolled back rather than kept; a save already visible to others
(`storage::submit`) stays queued, its caller is told it is not saved yet, and
the writer keeps trying with backoff (at most 5 seconds apart) for as long as
it takes, writing queued saves in order once the disk takes writes again. The
first save after a refusal compacts. Only an unconfirmed write (a rename or
directory sync failed, or a failed append could not be rolled back) halts the
writer: every later save is refused and its waiters stop Control, as before.
If a compaction's snapshot is durable but its log cannot restart, the writer
says so on stderr and the next save compacts again.

## Limits and next steps

Most saves still wait for durability under the write lock. Before more callers
release the lock before waiting, add durable intent and typed effect identity carrying
source generation and target revisions, revalidate authority and versions on
completion, and reconcile unknown outcomes. A plan from these operations cannot
be kept across another writer or an await: its rollback positions are only safe
under exclusive ownership. An async interpreter must settle an in-flight commit
before giving up that ownership, because dropping a future cannot undo a disk
write.

The application module still shares legacy service, delegation and bot record
types, and other handlers still take broad state. Web rendering remains in the
server. Next candidates are callback admission and bounded queries that let
rendering leave the lock; see [library boundaries](CRATE-BOUNDARIES.md) for the
client/server split.

Benchmark each migrated operation before and after, keeping startup,
allocations, resident memory, history scaling and lock/fsync latency as separate
measurements. Moving code is not a speed improvement. See
[PERFORMANCE.md](PERFORMANCE.md).
