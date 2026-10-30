# Projects and previews

Open a project on the machine running Control:

```sh
control tui --directory /absolute/path/to/project
```

In an existing TUI or web chat, use `/open /absolute/path/to/project`, or expand
**Open folder** in the web transcript. Review the folder and confirm **Trust
folder and open**. The TUI shows `/trust REQUEST` for the same confirmation.
Requests alone do not read or mount files. An agent can request access with
`control_project_request`; it cannot approve its own request. External MCP clients
can use `control.project.request` with a chat ID.

Control imports the current files, including uncommitted edits, into a private
Git worktree. The original checkout and its Git metadata remain unchanged.
Each attempt and child conversation gets independent writable files and Git
metadata. Project history is retained, but the source remote is removed from the
container's private clone. No automatic host merge, push or deployment occurs.
The completed worktree's local path is shown in the chat and TUI for review.

Git needs relative-worktree support; the Nix development environment provides it.
Plain folders get a private initial Git baseline. This first version rejects
symlinks and special files rather than following them into other host folders.
Control configuration/state, known credential files, environment files and common
generated directories are excluded from the file import. Git history is retained,
so only trust repositories whose history you intend to expose. An import is bounded to 1 GiB, 100,000
entries, depth 64 and 2,048 entries in a directory. A failed import grants nothing.

Project checkpoints retain full files, executable modes and private Git metadata.
They are separate from bounded artifact previews. Successful matching attempts
advance the project head; failures and interrupted attempts retain their work
without replacing it. Retries and design continuations keep their captured
baseline. APFS copies use independent copy-on-write files; writable hardlinks
are never shared between agents or with the original checkout.

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
