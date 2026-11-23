# History journals

Turns must not get slower as history grows. Rewriting and syncing a whole
`state.json` on every change costs time proportional to all history ever kept,
and most of those bytes are finished work (tool activity, objectives, results,
continuations, messages). Control therefore keeps finished history in
append-only files and saves only what changed.

## Chat journals

    state/history/<chat-id>.jsonl    one JSON object per line, never rewritten

- **Append-only.** Recording an entry writes and syncs one line, so its cost is
  the size of the entry. Positions are line numbers and never move.
- **Inspectable.** `cat`, `tail -f` and `jq` read it; no binary format, no
  database. A torn final line from a crash is dropped on open.
- **Pluggable.** `control_journal::Journal` (`append`, `len`, `read`) is the
  seam. `FileJournal` stores JSON lines, `Memory` serves tests and
  `Typed<J, T>` adds serde. Sealed or compressed segments for cold history
  could implement the same trait.
- **Comonadic.** A journal is a `control_comonad::Space`, so a `Store` can focus
  any position and `lens::cut` splits a history into O(log n) fixed blocks:
  verbatim near the focus, coarser farther back.

What lives in journals:

- **Tool activity.** Per-attempt activity is appended to the chat's journal as
  it happens; the attempt keeps only counts and the journal position.
- **Finished run text.** Run records stay in the state (spawn batches, lineage,
  approvals and inheritance look them up), but their bulky text (objective,
  result, lens context, continuation) is a `history::Spilled<T>` field: full in
  memory, saved as a journal position, and saved in full again after any edit.
  A finished run is one journal line committed atomically with its save: a
  failed save truncates the line, and load drops lines no save referenced.
- **Outbox** (`state/history/_outbox.jsonl`, `crates/delivery/src/lib.rs`). Control appends
  typed delivery events when a run finishes or waits. Frontends (currently the
  Discord bridge) follow it with their own cursor and never read state or chat
  journals. Events are at least once; followers deduplicate by run and status.
  Discord also sends a per-receipt message nonce, so a send repeated after a
  crash is dropped by Discord.

## Reading a conversation

`src/conversation.rs` is the one way to read a chat. `Conversation` indexes its
runs once and is a comonad `Space`. A `Compactor` chooses what a model turn
sees; set it per node in `history.json` in the configuration directory:

| `compactor` | Behaviour |
| --- | --- |
| `recent` (default) | Newest entries within a 20,000-character budget. |
| `passthrough` | Every earlier entry; the provider's own compaction decides. |
| `lens` | Newest `verbatim` entries (default 32) whole; older history as fixed `lens::cut` blocks, each shown by its summary. |

Example: `{"compactor": "lens", "verbatim": 32}`. Missing or invalid settings
keep `recent`. The `lens` compactor has no summary source yet (its `Summaries`
hook is `NoSummaries`), so it currently omits older blocks. Chats selected for
model-summarized [history compaction](LOCAL-MODELS.md) use that summary view
instead, falling back to the node compactor when no valid view exists.
Whichever view serves a chat, the operator's [Memory Tree edits](LOCAL-MODELS.md#editing-the-memorytree)
(selected summary revisions, pinned entries, dropped ranges) apply on top. They
are data saved with the chat; the journal is never rewritten.

## State checkpoints: a snapshot plus a patch journal

A checkpoint does not rewrite `state.json`. It appends what changed:

    state/state.json   snapshot: the state, plus "_snapshot" (a name) and "_schema"
    state/state.log    FileJournal: a header naming the snapshot, then one line per save

- **One save, one line.** `control_journal::snapshot::encode` serializes the
  state once (the same bytes as `serde_json::to_vec`), recording the ranges of
  members and elements inside containers of at least 256 bytes. `diff` compares
  the previous encoding with the new one byte for byte and descends only into
  ranges that changed, emitting JSON-Patch-like edits: set, remove member,
  splice array. A new chat message is a splice of that message, not the chat; a
  run's status is a set of one string. An unchanged save writes nothing.
  Diffing is a pure function of two encodings; nothing is parsed.
- **Cached runs.** `Run` is `history::Cached<Record>`: a save serializes a run
  once and copies its saved bytes until the run is next borrowed mutably (any
  `&mut` access forgets them), so encoding costs what changed plus a copy of
  the rest. Debug builds check every save against a fresh, uncached
  serialization.
- **Transactions.** Everything one save changed (a run, its chat, a receipt, a
  watch cursor) is one line, `{"sum":"<fnv-1a>","ops":[…]}`. The line is the
  transaction record and its newline the commit marker. Recovery replays the
  longest prefix of committed lines; a torn line, or a complete line whose
  checksum fails, ends the prefix and is truncated with everything after it.
  A crash at any byte therefore recovers the state before or after the save,
  never part of it (tested at every byte of a multi-entity save).
- **Durability.** A save returns after its line is written and synced
  (`FileJournal::append`; Rust's `sync_data`, a full flush on macOS), under the
  state mutex and the `state.lock` single writer. Receipts, idempotency keys and
  acknowledgements are therefore durable before the tool call replies. A failed
  append is truncated and reported as not replaced (callers roll back); if the
  truncation also fails the outcome is unconfirmed and Control stops, as for an
  unconfirmed snapshot rename.
- **Compaction.** When the log reaches the snapshot's size (at least 4 MiB),
  the next save compacts: the state goes to `state.json.tmp`, which is synced,
  renamed over `state.json`, and the directory synced; only then is the log
  restarted with a header naming the new snapshot. A crash between those steps
  leaves the old log, whose header names the old snapshot, so it is ignored
  (the new snapshot already contains it). Replay is bounded by the snapshot's
  size, and compaction at most doubles the bytes the log wrote.
- **Restart.** A clean stop compacts, so `state.json` alone is the whole state.
  Startup adopts that snapshot as the diff base without writing, indexing the
  file's bytes rather than re-encoding the state; after a crash it replays the
  log and compacts once.
- **Readers** that do not own the state (the Discord bridge, federation export)
  read snapshot plus log through `control_config::read_state` (or
  `read_state_in` for a given directory), typed as any partial view of the
  state, without a lock and without repairing anything.

### Inspecting the state

Both files are plain JSON. `control doctor` reports their sizes and whether
snapshot plus log parse. After a clean stop `state.json` alone is the state.
While Control runs (or after a crash) recent saves are in the log:

    cd ~/.local/state/control
    jq '.chats | length' state.json                 # the snapshot
    jq -c 'del(.ops)' state.log | head -1           # header: which snapshot, schema
    tail -n +2 state.log | jq -c '.ops[] | [.[0], .[1]]'   # each save: kind and path
    jq -r ._snapshot state.json                     # must match the header's "snapshot"

Edits are `["s", path, value]` (set), `["r", path]` (remove member) and
`["a", path, at, delete, [values…]]` (splice). A log whose header names another
snapshot is obsolete. To copy or back up a running node, copy both files, the
log second, or stop Control first. Do not edit either file while Control runs.

### Schema evolution and rollback

Snapshots and log headers carry the schema they were written with:
`{"version": N, "lenses": [[…step 1…], …]}`. Shape changes are small
bidirectional lenses after Ink & Switch's
[Cambria](https://www.inkandswitch.com/cambria/) (`control_journal::lens`:
rename, add/remove with a default, wrap/head, `in` and `map`). Reading
translates the snapshot as a document and every logged patch edit by edit, so
a log written by another version replays without being rewritten. The lenses
are stored as data in each snapshot, so an older build with this module walks a
newer build's steps backwards.

| Version | Change |
| --- | --- |
| 0 | Monolithic `state.json`. |
| 1 | Adds the log without changing the state's shape (no lenses). |
| 2 | Archived runs leave the state (an `External` step: nothing to translate forward, no way back). |

Departures from Cambria: history is linear (one step per version), not a lens
graph; there is no hoist/plunge or value conversion until a migration needs
one; and moves between documents (finished runs leaving the state) are not
lenses, as Cambria also leaves them open.

A `state.json` without `_snapshot` is read as version 0 and compacted on first
start. To roll back to a build without the log, stop Control cleanly (the stop
compacts) and start the older build; it ignores `_snapshot` and `_schema`.
After a crash, start and stop this build once first so the log is folded into
the snapshot. If an older build then rewrites `state.json`, the stale log no
longer names it and is ignored. Builds with lens support but without archives
refuse version 2 rather than silently miss archived runs; builds without the
log see only the hot runs, and the archived ones stay in `history/runs/`.

### Run archive

The log is a `FileJournal` as a `Space` whose position *i* is the snapshot with
the first *i* patches applied; the running server is a `Store` focused at the
end. Compaction has the shape of `lens::cut`: recent positions stay verbatim
near the focus and everything older folds into one immutable block, the
snapshot. Finished runs are moved out of the state the same way:

    state/history/runs/<start>-<level>.jsonl   one saved run per line, never rewritten

- **Which runs.** `application::runs::status` decides statuses in one place:
  `active` (queued, running, cancelling, waiting), `finished` (everything
  else), `resumable` (blocked, failed, interrupted, cancelled) and
  `superseded` (continued). A run is archivable when it is finished, not
  resumable, outside the newest 512 runs, unchanged since its last save and
  not referenced by a run that stays hot (persona root or parent, delegation
  child or continuation, spawned children).
- **Blocks.** Archivable runs move 64 or more at a time. A block covers
  archive positions `start .. start + 2^level`, aligned like `lens::cut`
  blocks; two adjacent blocks of one level merge into their parent, so the
  archive has O(log n) blocks. The state keeps only the list (`_archive`).
- **Commit.** A block is written to a temporary file, synced, renamed and the
  directory synced before the save that references it; that save commits it.
  A failed save unmarks the runs and deletes the new blocks. Blocks no
  committed save references (a crash in between) are deleted at load, and
  blocks a merge replaced only after the merge's save commits.
- **Lookups.** In memory every run stays in `Runtime::runs`, in creation
  order, so lookups by id, views, recovery and history checks are unchanged;
  an archived run is only left out of the state's encoding. Startup reads the
  blocks back eagerly.
- **Un-archiving.** Any mutable access to an archived run (a continuation
  recording itself on it, a delivery update) un-archives it: it is saved in the
  state again and shadows its block copy, which the next merge drops. Filling
  journal-backed text back in at load is not a change, so it keeps runs
  archived. A run archived again later is in two blocks until a merge; load and
  merge keep the newer block's copy.
- Readers outside the server (`config::read_state`) see only the hot runs.

Old chat messages are not archived yet; they are candidates for the same scheme.

## Reading activity for continuity

Activity reads distinguish a successful empty record from missing, invalid or
unavailable evidence. The API's hydrated attempt view adds `history_error` on a
failed read and keeps its `journal` reference; successful responses keep their
shape. Web and TUI show the warning even when tool details are folded. Legacy
handoff context, delegated feedback and outbound notifications also surface
read failures; they never invent a tool event or infer that nothing changed. A
later successful read clears the warning; reads never modify the attempt,
published checkpoint or processing cursors. Errors name the journal and entry
with a bounded cause rather than quoting corrupt contents. Startup hydration
still rejects missing run text or handoff state.

Codex and Claude CLI attempts start fresh processes; the shared conversation
projection supplies context, and neither provider's native session is the
source of truth. At admission, ordinary agents and bots use the same task view:
typed monitoring wakes contribute explicit progress or failures, while raw
history keeps every event at its original position. The original assignment
and committed working notes accompany the compactor's selection, which is not
trimmed a second time, so `passthrough` still leaves compression to the
provider. See [turn continuity](TURN-CONTINUITY.md).

Local chat messages accept up to 20,000 Unicode characters
(`CHAT_MESSAGE_CHARS`) without truncating the saved instruction or current
request. Web, TUI and operator MCP accept this limit; Discord and room posts have
their own bounds. HTTP chat forms and operator MCP allow 256 KiB of encoded
input (`CHAT_REQUEST_BYTES`).

The legacy prompt path (`chat::prompt`) adds the previous attempt's last six
nonempty command, tool or file-change observations (128 characters of label and
384 of output each), marked as untrusted evidence. This is a handoff, not the
tool transcript; tools remain available to inspect current state. Admissions
under the continuity protocol use checkpointed working notes and callback
receipts instead.

The task view does not renumber the conversation `Space`, and saving notes does
not replace history with a summary. Projection is pure; the prompt adapter
reads archived activity through the journal interface. That read is not bounded
by the output limit: hydration decodes the whole prior record, and a cold
journal open scans the file for line offsets. See
[performance checks](PERFORMANCE.md).

## Measurements

Recording one ~1.7 KB entry as history grows
(`cargo run --release -p control-journal --example bench [dir]`; Linux, pinned
build image):

| history | journal append | whole rewrite | read | open scan |
|---|---|---|---|---|
| 1 k | 1.4 ms | 3.2 ms | 0.5 µs | 0.6 ms |
| 10 k | 1.3 ms | 21.6 ms | 0.8 µs | 6.3 ms |
| 100 k | 1.1 ms | 272 ms | 1.0 µs | 61 ms |

Appends stay flat (one sync); the rewrite grows linearly. Opening scans the
file once for line offsets; a sidecar offset index could make that constant.

On the 600-turn workload (40 × 15 turns, 20 tool events per turn), moving
activity and finished run text into journals shrank the saved state from
7.1 MB to 0.58 MB while per-turn time held or improved. The 15-turn profile is
`fifteen_turn_conversations_workload` in `src/session_http/tests.rs`.
