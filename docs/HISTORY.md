# History journals

Turns must not get slower as history grows. Today every change re-serializes
and fsyncs the whole `state.json`; a 15-turn profile spent 91% of Control's
per-turn time in that save, and the cost grows with all history ever kept.
Most of the bytes are finished work: per-attempt tool activity was 40% of a
real 5.9 MB state, then objectives, continuations, results and messages.

## Shape

History moves into append-only JSON-lines files, a layer of plain files that
stay open for inspection:

    state/history/<chat-id>.jsonl    one JSON object per line, never rewritten

- **Append-only.** Recording a turn writes and syncs one line, so its cost is
  the size of the turn. Positions are line numbers and never move.
- **Inspectable.** `cat`, `tail -f` and `jq` read it; no binary format, no
  database. A torn final line from a crash is dropped on open.
- **Pluggable.** `control_journal::Journal` (`append`, `len`, `read`) is the
  seam: JSON-lines files now, sealed or compressed segments later (a
  squashfs-like archive of cold history), memory in tests.
- **Comonadic.** A journal is a `control_comonad::Space`, so a `Store` focuses
  any position and `lens::cut` gives an unbounded history as O(log n) blocks:
  verbatim near the focus, summarized farther back. Summaries are journal
  entries too, written once per fixed block (UniiChat-style infinite history).
- **Tree-shaped.** Like Claude Code and pi sessions (`uuid`/`parentUuid`,
  `id`/`parentId`), entries name their parent, so branches live in one file and
  `control_comonad::Tree` rebuilds the conversation tree in one pass.

`state.json` keeps only what is live (active runs, chats, cursors and
small indexes into journals), so a save costs the active set, not the past.

## Phases

1. **A — journal crate** (`crates/journal`): `Journal`, `FileJournal`,
   `Memory`, `Typed<T>`; comonad `Space` generalized to owned items.
2. **B — activity**: per-attempt tool activity is appended to the chat's
   journal as it happens; state keeps counts and the transcript reads the tail.
3. **C — finished run text**: run records stay in state (many lookups depend on
   finished runs: spawn batches, lineage, approvals, inheritance), but their
   bulky text (objective, result, lens context, continuation) moves to the
   journal. Fields are `Spilled<T>`: always full in memory, saved as a journal
   position, and saved in full again after any edit. A finished run is one
   journal line (text and last activity) committed atomically with its save:
   a failed save truncates the line, and load drops lines no save referenced.
4. **D1 — conversation interface** (`src/conversation.rs`): the one way to read a
   chat. `Conversation` indexes its runs once and is a comonad `Space`;
   `Compactor` chooses what a model turn sees: `Recent` (default, the
   20,000-character budget),
   `Passthrough` (provider compaction decides) or `Lens` (unbounded history over `lens::cut`, older blocks shown
   by summaries through a `Summaries` hook). Set per node in `history.json`.
5. **D2 — outbox** (`src/outbox.rs`, `history/_outbox.jsonl`): typed delivery
   events Control appends when a run finishes or waits; frontends (Discord now;
   Telegram, Matrix, Stoat later) follow it with their own cursor and never
   read state or chat journals. Events are at least once; followers deduplicate
   by run and status (Discord also sends a per-receipt message nonce, so a send
   repeated after a crash is dropped by Discord). Start with fresh state when
   upgrading: there is no load-time migration or baseline of older state; runs
   a crash interrupted are archived when recovery marks them interrupted.

Each phase is profiled with the 15-turn workload (`fifteen_turn_conversations_workload`).

## State checkpoints: a snapshot plus a patch journal

A checkpoint no longer rewrites `state.json`. It writes what changed:

    state/state.json   snapshot: the state, plus "_snapshot" (a name) and "_schema"
    state/state.log    FileJournal: a header naming the snapshot, then one line per save

- **One save, one line.** `control_journal::snapshot::encode` serializes the
  state once (the same bytes as `serde_json::to_vec`), recording the ranges of
  members and elements inside containers of at least 256 bytes. `diff` compares
  the previous encoding with the new one byte for byte and descends only into
  ranges that changed, emitting path edits in the spirit of JSON Patch: set,
  remove member, splice array. A new chat message is a splice of that message,
  not the chat; a run's status is a set of one string. An unchanged save
  writes nothing. Diffing is a pure function of two values; nothing is parsed.
- **Cached runs.** `Run` is `history::Cached<Record>`: a save serializes a run
  once and copies its saved bytes until the run is next borrowed mutably (any
  `&mut` access forgets them), so encoding costs what changed plus a copy of
  the rest. Diffing indexes a run's saved bytes when it must look inside one.
  Debug builds check every save against a fresh, uncached serialization.
- **Transactions.** Everything one save changed (a run, its chat, a receipt, a
  watch cursor) is one line, `{"sum":"<fnv-1a>","ops":[…]}`. The line is the
  transaction record and its newline the commit marker. Recovery replays the
  longest prefix of committed lines; a torn line, or a complete line whose
  checksum fails, ends the prefix and is truncated with everything after it.
  A crash at any byte therefore recovers the state before or after the save,
  never part of it (tested at every byte of a multi-entity save).
- **Durability.** A save returns after its line is written and synced
  (`FileJournal::append`, `F_FULLFSYNC` on macOS), under the existing state
  mutex and `state.lock` single writer. Receipts, idempotency keys and
  acknowledgements are therefore durable before the tool call replies. A failed
  append is truncated and reported as not replaced (callers roll back); if the
  truncation also fails the outcome is unconfirmed and Control stops, exactly as
  for an unconfirmed snapshot rename.
- **Compaction.** When the log grows to the snapshot's size (at least 4 MiB),
  the next save compacts: the state goes to `state.json.tmp`, which is synced,
  renamed over `state.json`, and the directory synced; only then is the log
  restarted with a header naming the new snapshot. A crash between those steps
  leaves the old log, whose header names the old snapshot, so it is ignored
  (the new snapshot already contains it). Replay is bounded by the snapshot's
  size and compaction at most doubles the bytes the log wrote.
- **Restart.** A clean stop compacts, so `state.json` alone is the whole state.
  Startup adopts that snapshot as the diff base without writing, indexing
  the file's bytes rather than re-encoding the state; after a crash it
  replays the log and compacts once.
- **Readers** that do not own the state (the Discord bridge, federation export)
  read snapshot plus log through one loader, `control::config::read_state` (or
  `read_state_in` for a given directory), typed as any partial view of the state, without a lock and
  without repairing anything.

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
translates on read: the snapshot as a document and every logged patch edit by
edit, so a log written by another version replays without being rewritten. The
lenses are stored as data in each snapshot, so an older build that has this
module walks a newer build's steps backwards. Version 0 is the monolithic
`state.json`; version 1 adds the log without changing the state's shape, so its
step has no lenses.

Departures from Cambria: history is linear (one step per version), not a lens
graph; there is no hoist/plunge or value conversion until a migration needs
one; and moves between documents (finished runs leaving the state) are not
lenses, as Cambria also leaves them open.

Migration from the monolithic file is automatic: a `state.json` with no
`_snapshot` member is read as version 0 and compacted on first start. Rollback
to a build from before the log: stop Control cleanly (the stop compacts) and
start the older build; it ignores `_snapshot` and `_schema`. After a crash,
start and stop this build once first, so the log is folded into the snapshot.
If an older build then rewrites `state.json`, the stale log no longer names it
and is ignored.

Schema version 2 moves archived runs out of the state (an `External` lens step:
nothing to translate forward, no way back). Builds with lens support but
without archives refuse to read it rather than silently miss those runs.
Builds from before the log read `state.json` and see only the hot runs; the
archived ones stay in `history/runs/` for a later upgrade.

### How this maps onto Journal, Store and lens

The log is a `FileJournal`, with its format and crash semantics: JSON lines,
appends write and sync only the new line, positions never move, a torn tail is
dropped, `truncate` removes what never committed. As a `Space` its positions are
states: position *i* is the snapshot with the first *i* patches applied, and
the running server is a `Store` focused at the end (`extract` is the current
state). Compaction has the shape of `lens::cut`: the newest positions stay
verbatim (one patch each) near the focus, and everything older is folded into
one complete block, the snapshot, that never changes once written; a new
compaction writes a new block under a new name rather than editing the old one.

Finished runs are positions far from the focus too, and leave the state for
cut-shaped blocks:

    state/history/runs/<start>-<level>.jsonl   one saved run per line, never rewritten

- **Which runs.** `application::runs::status` decides statuses in one place:
  `active` (queued, running, cancelling, waiting), `finished` (everything
  else), `resumable` (blocked, failed, interrupted, cancelled) and
  `superseded` (continued). A run is archivable when it is finished, not
  resumable, outside the newest 512 runs, unchanged since its last save and
  not pointed at by a run that stays hot (persona root or parent, delegation
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
  blocks back (eagerly; replay and load stay near 100 ms).
- **Un-archiving.** Any mutable access to an archived run (a continuation
  recording itself on it, a delivery update) un-archives it: it is saved in the
  state again and shadows its block copy, which the next merge drops. Filling
  journal-backed text back in at load is not a change (the save writes the
  journal position), so it keeps runs archived. A run archived again later is
  in two blocks until a merge: load and merge keep the newer block's copy.
- Readers outside the server (`config::read_state`) see only the state's hot
  runs; none of them read runs today.

Old chat messages are the next positions to move the same way.

## Turn continuity

Activity reads distinguish a successful empty record from missing, invalid or
unavailable evidence. The API's hydrated attempt view adds `history_error` on a
failed read and retains its `journal` reference; successful responses keep their
existing shape. Web and TUI show the warning even when tool details are folded.
Legacy handoff context, delegated feedback and outbound notifications also expose
read failures when consuming activity. They do not invent a tool event or infer
that no changes occurred. A later successful read clears the warning in the view;
reads do not modify the attempt, published checkpoint or processing cursors.
Error messages name the logical journal and entry, with a bounded cause rather
than quoting corrupt contents. Existing startup hydration still rejects missing
run text or handoff state; this does not weaken that recovery boundary.

Codex and Claude CLI attempts start fresh processes. Their shared conversation
projection supplies context; neither provider's native session is the source of
truth. At admission, ordinary agents and bots use the same task view: typed
monitoring wakes contribute explicit progress or failures while raw history
retains every event at its original position. The original assignment and
committed working notes accompany the configured compactor's chronological
selection. Assembly does not trim its selection a second time. Passthrough
therefore still delegates compression to the provider.

Local messages accept up to 20,000 Unicode characters without truncating the
saved instruction or the current request.
Web, TUI and operator MCP accept this limit; Discord and room posts retain their
separate bounds. HTTP chat forms and operator MCP allow 256 KiB for encoded input.

Legacy attempts without the continuity protocol receive the previous attempt's last six nonempty command, tool or
file-change observations (128 characters of label and 384 of output each).
These are untrusted evidence, not authority or private model reasoning. They
provide a handoff, not the entire tool transcript; tools remain available to
inspect current resource state. New admissions use checkpointed working notes
and callback receipts instead of this heuristic tail.

Neither the task view nor saved subscription origins renumber the conversation
`Space`. `Store::seek` can refocus retained positions after a journal reload;
`Lens` and its `Summaries` hook keep the same fixed block coordinates for an edge
compacter. Saving notes does not replace the tape with a summary. The persistence
regression covers focus, raw/task positions, synthetic edge summaries and progress
before and after archive/save/recover/hydration. Background summary generation is
not implemented by this continuity change.

Projection is pure; the prompt adapter reads archived activity through the
existing journal interface. The output limit does not bound that read: activity
hydration currently decodes the entire prior record, and a cold journal open
scans the file for offsets. Large observations and cold histories therefore
remain separate performance concerns. See [performance checks](PERFORMANCE.md).

## Measured (phase A)

Recording one ~1.7 KB turn as history grows, in the pinned build image:

| history | journal append | whole rewrite | read | open scan |
|---|---|---|---|---|
| 1 k | 1.4 ms | 3.2 ms | 0.5 µs | 0.6 ms |
| 10 k | 1.3 ms | 21.6 ms | 0.8 µs | 6.3 ms |
| 100 k | 1.1 ms | 272 ms | 1.0 µs | 61 ms |

Appends stay flat (one fsync); the rewrite grows linearly. Opening scans the
file once for line offsets; a sidecar offset index can make that constant.

## Measured (phase B)

600 turns (40 × 15) with 20 tool events per turn, macOS release build:

| | per turn | state after 600 turns |
|---|---|---|
| before | 26.3 ms | 7.1 MB |
| phase B | 26.6 ms | 2.0 MB |

On macOS a durable sync is a full flush and syncs dominate a turn, so moving
activity out trades serialization for one small journal sync: time holds while
state shrinks 71%. On Linux (pinned images, same workload) a turn went from
14.0–15.0 ms to 11.7–13.3 ms.

## Measured (phase C)

Same workload on Linux, four alternating runs each:

| | per turn (mean) | saved state after 600 turns |
|---|---|---|
| phase B | 11.0 ms | 2.0 MB |
| phase C | 10.4 ms | 0.58 MB |

Finishing a run is one journal line and one sync, so the smaller save is a net
win that grows with history.
