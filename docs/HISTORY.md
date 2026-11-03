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
4. **D — conversation history interface**: chat messages and results live in
   the journal behind one frontend-neutral interface (append, read a range or
   tail, follow). Web, TUI, Discord and future bridges (Telegram, Matrix, Stoat)
   are adapters over it and never read storage files; Discord stops reading
   `state.json` directly. The transcript reads through `lens::cut`, and
   summaries make history unbounded.

Each phase is profiled with the 15-turn workload (`fifteen_turn_conversations_workload`).

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
