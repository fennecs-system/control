# Shared knowledge

Three tools read and propose knowledge. The operator bridge and HTTP MCP use the
dotted names; workers use the underscore aliases.

| Tool | Purpose |
| --- | --- |
| `control.knowledge.search` | Up to 10 matches. Every whitespace-separated query word (1–200 characters in total, case-insensitive, any order) must appear in the title or body. Results keep stable document/claim order. |
| `control.knowledge.read` | Read a search result by ID, in pages of at most 4,000 Unicode characters (offset up to 32,000). |
| `control.knowledge.propose` | Submit one verified, reusable claim for operator review. Returns at once. |

## What a worker can read

A worker reads the document revisions its lenses captured when its run was
prepared. Later note or lens edits do not reach a running attempt. Operator MCP
calls read current documents. All authorized workers can read claims the local
operator has admitted. Documents are notes, not verified facts; a claim's scope
describes where it applies and grants no authority.

## Proposals

A proposal has a claim (280 characters), evidence (1,000), scope (300) and a
recheck condition (`validity`, 300). Control records the actor, chat, attempt,
request identity and captured lens/document revisions; the worker cannot set or
change that provenance. A retry with the same request identity returns the same
result; changed contents are rejected. A node retains at most 256 proposals. A
failed checkpoint rolls the proposal back.

On the operator bridge, `chat` must name a local operator chat; the actor is the
local operator. Judges can read knowledge but cannot propose. Inactive or stale
worker mailboxes cannot propose.

## Review

Pending proposals appear under **Knowledge → Review** and, like a design
question, as an operator request: in `chat_requests`, the DM's Requests drawer,
Manage → Permissions, the TUI and Discord notifications. The proposing agent
never waits: proposing returns at once and its turn carries on. Only an
authenticated local operator can accept or reject a proposal: on the Review page,
with `/knowledge accept|reject claim:N` in the TUI, by replying accept or reject on
Discord, or with the chat MCP's `chat_decide` when Manage → Permissions enables
knowledge decisions (off by default). Every surface posts the same Review form,
so they share one state, and a decision on a proposal that is no longer pending
is refused. Admission is final and does not overwrite
documents; a failed review checkpoint restores the pending state. Accepted claims
keep their evidence, scope, recheck condition and compact provenance. Evidence is
for the reviewer; the tool does not verify it. No background model calls occur.
