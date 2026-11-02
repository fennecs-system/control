# Projects and previews

Open a project on the machine running Control:

```sh
control tui --directory /absolute/path/to/project
```

In an existing TUI or web chat, use `/open /absolute/path/to/project`, or expand
**Open folder** in the web transcript. Review the folder and confirm **Trust
folder and open**. In the TUI, review the prompt at the end of the conversation
and enter `/trust` (or `/trust REQUEST`) to confirm without opening a browser.
`/open` reviews the current request; `/open /another/path` replaces it.
Requests alone do not read or mount files. An agent can request access with
`control_project_request`; it cannot approve its own request. External MCP clients
can use `control.project.request` with a chat ID.

Each approved folder appears as `/workspace/<folder-name>`. Repeat `/open` to add
another folder to the same chat; approvals stay beside the corresponding request.
Use `/open --as api2 /another/path/api` for duplicate names, or edit the workspace
name in the approval form. MCP requests accept an optional `name`. Each folder
has its own Git metadata. Pending or failed additions keep already-approved
folders and their edits; an import error appears beside its request for retry.
Existing single-folder chats retain their layout until another folder is approved.

Control imports the current files, including uncommitted edits, into a private
Git worktree. The original checkout and its Git metadata remain unchanged.
Each attempt and child conversation gets independent writable files and Git
metadata. Project history is retained, but the source remote is removed from the
container's private clone. No automatic host merge, push or deployment occurs.
The completed worktree's local path is shown in the chat and TUI for review.

Git needs relative-worktree support; the Nix development environment provides it.
Plain folders get a private initial Git baseline. Symlinks, including dangling
links and links into the Nix store, are skipped without following their targets.
Special files are rejected.
Control configuration/state, known credential files, environment files and common
generated directories are excluded from the file import. Git history is retained,
so only trust repositories whose history you intend to expose. Project imports and
checkpoints have no byte or entry-count cap; available node storage is the limit.
The depth-64 guard remains. A failed import grants nothing. Approved folders are
mounted separately and do not consume a container writable-layer quota. Isolated
project copies and checkpoints still use node storage; APFS clones share unchanged
blocks until modified.

Project checkpoints retain full files, executable modes and private Git metadata.
They are separate from bounded artifact previews. Successful matching attempts
advance the project head; failures and interrupted attempts retain their work
without replacing it. Retries and design continuations keep their captured
baseline. APFS copies use independent copy-on-write files; writable hardlinks
are never shared between agents or with the original checkout.

Working copies are disposable views of the captured checkpoint; they do not need
per-file disk flushes before execution. Published checkpoints still flush their
files before advancing the project head. Preparation and checkpoint durations
appear in recent activity in the web chat and TUI.

## Local previews

Trusting a project also allows its agents to start local previews:

- `control_preview_start`: a server command and its container port.
- `control_preview_stop`: stop the attempt's preview.

The selected backend's pinned Nix image runs the service without credentials or
external networking. Its project mount is read-only; builds and caches can use
`/tmp` (`CARGO_TARGET_DIR=/tmp/target`). Control forwards HTTP traffic from a
loopback listener to that container port using a small Rust stdin/stdout bridge.
The bridge is compiled from bundled source by the pinned image's Rust compiler.

**Open preview** appears in chat; the URL also appears in the TUI. Preview
hostnames are separate from Control's operator origin. The proxy rejects requests
for Control's hostname and requests carrying its operator cookie. Ingress is
limited to 16 connections per service; headers are bounded to 32 KiB. Connections
have a five-minute input idle limit and a 30-second output write timeout.

Services survive a completed agent turn. On restart, Control verifies the
persisted container identity and mount policy before rebinding the same local
port. A busy port or changed resource becomes unavailable; it is never adopted
or overwritten. Stop services from their chat. At most 16 active previews are
allowed per node. Container logs are bounded and retained until the service is
stopped.

## Transport boundary

A project grant authorizes files; a preview service identifies one target and
port. A lens selects which tools are offered; it does not grant host access.
Transport is separate from both. SSH forwarding, Tailscale or an authenticated
tunnel can later publish the same scoped service without giving the agent a host
socket or arbitrary proxy target. Remote publishing, native file pickers,
in-place host edits and Jujutsu integration are not implemented by this first cut.

## Environment boundary

Base images are pinned and mounted read-only. Worker changes belong in the
private workspace or temporary filesystem; there is no container-engine socket
or host Nix daemon available to agents. The current images are built with Nix
but do not include the `nix` command. Additional package environments should be
provided through tools, with explicit capabilities rather than guessed commands.

Future in-worker Nix support must keep writable stores, profiles and build state
private to the worker. It must not commit a worker into a shared image or publish
to a shared cache implicitly. Fetching packages and publishing cache entries are
separate permissions. Cache reuse can improve efficiency without making a
worker's writable state the next worker's base.
