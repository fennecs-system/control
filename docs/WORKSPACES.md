# Projects and previews

## Opening a folder

Open a project on the machine running Control:

```sh
control tui --directory /absolute/path/to/project
```

In an existing TUI or web chat, use `/open /absolute/path/to/project`, or expand
**Open folder** in the web transcript, then review the folder and confirm **Trust
folder and open**. In the TUI, review the request at the end of the conversation
and enter `/trust` (or `/trust REQUEST`). `/open` alone shows the current request;
`/open /another/path` replaces it. Your `/open` checks that the path is an
existing folder and fails at once if not, so a typo never becomes a request.

`/open` also takes a list. Separate paths with spaces (`/open /a/api /b/web`),
commas, or put one per line (Shift+Enter on the web, Alt+Enter in the TUI). A
space separates paths only when the next path starts with `/` or `~`, so a single
path with spaces such as `/open /Users/me/My Project` still opens as one folder;
quote a path whose name has a space before a `/` (`/open "/data/a /b" /c`). With
commas or new lines each item is one path, spaces included. The reply lists every
path with its result: `→ /workspace/NAME`, or why it was refused (missing, not a
folder, a name already in use). When every path passes they become one request
with one **Trust folders and open** decision, and each opens at its own
`/workspace/<name>`. If any path is refused nothing is requested, because a later
`/open` replaces the pending request; correct the list and send it again. Dismiss
a request and `/open` again to change its list. `--as NAME` takes a single path.

`~` and `~/path` mean the home directory of the user running Control on the node,
because paths are read on the node, not on the device you type from (the TUI can
connect to a remote server). Other relative paths are refused. Agents'
`control_project_request` still takes one absolute path and is never expanded.

**Dismiss** in the web request (or `/dismiss [REQUEST]` in the TUI) rejects the
displayed request. The chat returns to the folders it had before the request,
including agents' edits since; already approved folders keep working. Dismissal
names the request it rejects: from a stale page, or for a request that was
already dismissed, replaced or approved, it changes nothing and says so. An
approved request that is still importing cannot be dismissed; the copy is not
cancellable and opens when it finishes. The chat records a **Folder decisions**
notice, and an agent that asked resumes its finished turn, as for port and bot
decisions.

A request alone reads and mounts nothing. Agents can ask with
`control_project_request` but cannot approve their own request. External MCP
clients use `control.project.request` with a chat ID. Agent requests are never
checked against the host, so agents cannot learn which paths exist.

Each approved folder appears at `/workspace/<folder-name>`. Repeat `/open` to add
more folders to the same chat. For duplicate names use
`/open --as api2 /another/path/api`, edit the name in the approval form, or pass
`name` in an MCP request. Each folder keeps its own Git metadata. A pending or
failed addition leaves already approved folders and their edits in place.
Single-folder chats keep their layout until a second folder is approved.

Approval answers at once; the import runs in the background. The request shows
**Importing folder…** with progress, and the chat shows `Opened /workspace/<name>`
when it finishes. An import error appears beside the request, which stays
available to retry. Messages to the agent wait until the import finishes. A
second approval of the same request starts no second copy. The copy is staged
outside the published grants and moved into place only as the chat records it,
so a failed import leaves nothing behind. A restart interrupts the import and
grants nothing; the next start removes its staged copy.

## Isolated copies

Control imports the current files, including uncommitted edits, into a private Git
worktree. The original checkout and its Git metadata are unchanged. Each attempt
and child conversation gets its own writable files and Git metadata. History is
kept, but the source remote is removed from the private clone. Control never
merges, pushes or deploys to the host automatically. The finished worktree's local
path is shown in the chat and TUI for review.

- Git must support relative worktrees; the Nix development environment provides
  this. Plain folders get a private initial commit.
- Symlinks (including dangling links and links into the Nix store) are skipped
  without being followed. Special files are rejected. Directory depth is limited
  to 64.
- Control configuration and state, known credential files, environment files and
  common generated directories are excluded. Git history is included, so only
  trust repositories whose history you are willing to expose.
- There is no byte or entry cap on imports or checkpoints; node storage is the
  limit. Approved folders are mounted separately and do not count toward a
  container's writable-layer quota. On APFS, copies are copy-on-write clones that
  share unchanged blocks; writable hard links are never shared between agents or
  with the original checkout.

Project checkpoints keep full files, executable modes and private Git metadata,
separately from bounded artifact previews. A successful attempt advances the
project head; failed and interrupted attempts keep their work without replacing
it. Retries and design continuations keep their captured baseline. Working copies
are disposable views of a checkpoint and are not flushed file by file; published
checkpoints are flushed before the head advances. Preparation and checkpoint
times appear in recent activity.

## Local previews

Trusting a project also lets its agents serve local HTTP previews:

- `control_preview_start` takes a server `command` and its container `port`.
  Bind the server to `0.0.0.0`.
- `control_preview_stop` stops the attempt's preview.

The preview runs in the backend's pinned Nix image with no credentials and no
external network. The project mount is read-only; write builds and caches under
`/tmp` (`CARGO_TARGET_DIR=/tmp/target` is set). Control forwards HTTP from a
loopback listener to the container port through a small Rust stdin/stdout bridge,
compiled from bundled source by the image's Rust compiler.

**Open preview** appears in chat, and the URL appears in the TUI. Preview
hostnames are separate from Control's operator origin: the proxy rejects requests
for Control's hostname or carrying its operator cookie.

| Limit | Value |
| --- | --- |
| Active previews per node | 16 |
| Connections per preview | 16 |
| Request header size | 32 KiB |
| Input idle timeout | 5 minutes |
| Output write timeout | 30 seconds |

Previews outlive the agent turn. On restart, Control checks the saved container
identity and mount policy before rebinding the same local port; a busy port or
changed container makes the preview unavailable rather than adopting or
overwriting anything. Stop previews from their chat. Container logs are bounded
and kept until the preview stops.

For non-HTTP protocols, use [scoped TCP ports](PORTS.md).

## Boundaries

A project grant authorizes files. A preview identifies one container and port. A
lens selects which tools are offered; it grants no host access. Transport is
separate from all three: an SSH forward, Tailscale or an authenticated tunnel
could publish the same scoped preview later without giving the agent a host socket
or an arbitrary proxy target. Remote publishing, native file pickers, in-place
host edits and Jujutsu integration are not implemented.

Base images are pinned and read-only; agents get no container-engine socket or
host Nix daemon. Writable state stays in the private workspace or temporary
filesystem. See [worker profiles](WORKER-PROFILES.md) for the private Nix store in
Full images and [build environments](BUILD-ENVIRONMENTS.md) for declared build
images.
