# Shared knowledge

`control.knowledge.search` returns at most ten concise matches. Use the returned
ID with `control.knowledge.read` for text in pages of at most 4,000 Unicode
characters. Workers use the underscore aliases `control_knowledge_search` and
`control_knowledge_read`.

A worker can read the document revisions captured by its lenses when its run was
prepared. Editing a note or changing lenses later does not expose a new document
to that running attempt. Local operator MCP calls can read current documents.
Documents remain notes, not automatically verified facts. All authorized workers
can search and read claims explicitly admitted by the local operator; claim scope
describes applicability and does not grant authority.

`control_knowledge_propose` submits one claim (280 characters), evidence (1,000),
scope (300), and recheck condition (`validity`, 300). Control records the run's
actor, chat, attempt, request identity and captured lens/document revisions. The
worker cannot supply or change that provenance. A retry with the same request
identity returns the same result; changed contents are rejected. There is a node
limit of 256 retained proposals. Failed disk checkpoints roll back the proposal.

Pending proposals appear under **Knowledge → Review**. Only an authenticated
local operator can accept or reject them there. Admission is terminal and does
not overwrite documents. Failed review checkpoints restore the pending state.
Judges can read knowledge but cannot submit proposals. Inactive or stale worker
mailboxes cannot propose claims.

The native MCP bridge exposes the same three dotted tools. A local proposal
requires `chat` identifying a local operator chat. Its actor is the local
operator, rather than a worker identity; identical submissions are idempotent.

Accepted claims retain evidence, scope, recheck condition and compact provenance
when read. Evidence is supplied for operator review, not independently verified
by this tool. Automatic compaction, claim replacement and event notifications are
separate work; no background model calls occur here.
