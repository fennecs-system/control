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
3. **C — finished runs**: terminal runs move to their chat's journal, leaving a
   slim index (id, chat, kind, status, times) for lineage lookups.
4. **D — messages**: chat messages and results live in the journal; the
   transcript reads through `lens::cut`, and summaries make history unbounded.

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
