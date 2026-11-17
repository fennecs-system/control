# Tool packages

A tool package is a folder of code that a swarm built once and the operator has
admitted for reuse: a client, an adapter, a script set or a Nix flake. Control
stores each admitted version on the node and copies it into later runs that
opt in. Packages are files with provenance. Admitting one grants no host
access, network, credentials or new tool authority.

The examples below use `mc263-client`, a Minecraft client a swarm took about 20
minutes to build. A later run can reuse an admitted version instead. Workload
details such as the Minecraft protocol stay in the package and its README, not in
Control.

Packages reuse the [project folder](WORKSPACES.md) import rules and
[lens](LENSES.md) capture. Agent proposals and review, which would follow the
[knowledge](KNOWLEDGE-TOOLS.md) pattern, are planned in the
[swarm roadmap](SWARM-ROADMAP.md#l2l3-a-reviewed-self-improvement-loop).

## Store

Packages live under the node state directory (`$CONTROL_STATE_DIR/tool-packages`),
never in the checkout:

```text
tool-packages/<name>/repository.git      bare Git repository, one per package
tool-packages/<name>/versions/<n>/tree   immutable checkout of the admitted commit
tool-packages/<name>/versions/<n>/control-tool.json   manifest and provenance
```

The bare repository keeps history and one tag per admitted version (`v1`, `v2`).
It has no remote, hooks or credential helpers; every Git call runs with an empty
environment and no system or global config, as project import does. The
extracted tree lets an attempt copy a version with APFS clones (or one plain copy
on Linux) without running Git.

The node's JSON state keeps a small index of versions: name, version number,
commit, tag, the manifest summary, provenance and status. Files hold the bytes.
There is no database.

### Manifest

`control-tool.json` records:

| Field | Meaning |
| --- | --- |
| `name` | Package ID: lowercase letters, digits and dashes, at most 64 characters. |
| `version`, `commit`, `tag` | Version number, the full commit ID and its tag. |
| `files`, `bytes` | File count and total size of the version. |
| `description` | One line, at most 300 characters. |
| `usage` | How to use it, at most 2,000 characters. For example, where the folder must sit (`/workspace/castle`). |
| `setup` | Up to eight setup commands, such as `npm ci` or `nix build`. These are documentation. Control never runs them. |
| `entry` | Optional main file or README path inside the package. |
| `provenance` | Who admitted it and how, optional run/chat/agent references, evidence and a recheck condition. The host source path is shown only to the operator. |
| `admitted_at` | When it was admitted. |
| `revoked` | Present after revocation: who, when and why. |

A package may ship its own `control-tool.json` at the repository root with
`description`, `usage`, `setup` and `entry`. Operator-supplied fields override
it. Control writes the stored manifest; the package cannot set provenance,
version, commit or admission fields.

## Versions

An admitted version is an immutable commit plus a `vN` tag. Version numbers
increase per package and are never reused. Admitting the same commit again
returns the existing version; a revoked commit cannot be admitted again. The
optional `--commit` prefix makes an import fail unless `--rev` resolves to that
commit. A new commit becomes a new version; earlier
versions stay available, so a v2 light-data fix to `mc263-client` does not
change runs that captured v1.

Runs capture the exact version and commit, like lens and document revisions.
Changing the store, admitting v2 or editing the lens later does not change a
queued or running attempt's package.

## Lifecycle

```text
import (operator) ─▶ admitted ─▶ revoked (terminal)
```

- **Import (operator).** An authenticated local operator admits a Git repository
  folder on this node as a new package or version:

  ```sh
  control package import mc263-client ~/control-tools/mc263-client \
    --description "Native Minecraft Java 26.3 Mineflayer client and gateway" \
    --usage "Copy the folder to /workspace/castle. Start with client/README.md." \
    --setup "cd client && npm ci" \
    --evidence "Built by swarm run c-18e833b1f7ada920-121; fixture-check.cjs passes" \
    --recheck "When Mineflayer or minecraft-data ship official 26.3 support"
  ```

  The path must be absolute. The CLI calls the operator MCP tool
  `control.package.import` with the local operator session. The import reads the
  committed tree at `--rev` (default `HEAD`); uncommitted edits are not included.
  Git and copy work run without holding the state lock; the index entry is
  checkpointed afterwards and its files removed if the save fails. An operator
  import is itself the admission. `control package list [QUERY]` and
  `control package show NAME [VERSION]` inspect the store.

- **Revoke.** `control package revoke mc263-client 1 --reason "..."` (MCP
  `control.package.revoke`) marks a version revoked. Revocation is terminal.
  Its files stay for audit. New contexts cannot capture it. An attempt that
  starts after revocation does not receive the files, even if its run captured
  the version earlier. Files already copied into a running attempt are not
  removed. An unpinned lens reference falls back to the latest version that is
  still admitted; a pinned reference to a revoked version stops resolving, and
  sending to a chat with that lens fails until the operator edits the lens.

- **Discovery.** Workers call `control_packages_search` and
  `control_package_read`. Search and read cover only the versions their run
  captured, so a run that did not opt in sees nothing and rebuilds from scratch.
  Results are operator-reviewed evidence, not instructions: the same framing as
  captured knowledge documents. `control_package_read` returns the manifest and
  pages of the README (or another UTF-8 text file in the package, at most
  4,000 characters per call). The operator MCP tools `control.packages.search` and
  `control.package.read` see every version, including revoked ones.

## Opt in per run with a lens

A lens names the packages a run may use, next to its tools and documents:

```json
{"id":"minecraft-tools","name":"Minecraft tools","revision":1,
 "packages":["mc263-client@1"],"instructions":"Reuse the admitted 26.3 client."}
```

`mc263-client@1` pins version 1; `mc263-client` means the latest admitted version
when a message is sent. The **Lenses** page lists admitted packages as
checkboxes, and each checkbox pins the version shown.

A lens is the right place for this choice because it is the existing,
versioned, per-chat selection that every run already captures. Spawned workers,
design continuations and bot wakes inherit their parent's captured context, so
the whole team gets the same pinned version without a new setting. A run that
should rebuild from scratch simply leaves the lens out. Because the package is
named in context, the agent also learns what it has: the prompt names each
captured package, its version and where its files are.

Package choices are a context choice, not a grant. Package files are already
operator-admitted, credential-free files, so offering them needs no new authority.
Copying a package into a service still uses the existing service grants.

## Mounting into a run

When an attempt's workspace is prepared, Control copies each captured, still
admitted version from the store into:

```text
/workspace/.control-tools/<name>
```

This reuses the project-folder mechanism: the version's immutable tree is cloned
into the attempt's private workspace with the same no-follow copy as project
checkpoints (APFS copy-on-write on macOS). The store copy is never mounted
writable, so no run can change an admitted version. An attempt's own copy is a
disposable view. Edits to it stay in that attempt and are never written back.
The `.control-` prefix keeps it out of project checkpoints, published artifacts,
diffs and recovery snapshots, so a package does not grow every checkpoint. Each
attempt gets a fresh copy of the pinned version. Judges receive it in their
read-only source like the rest of the workspace.

Agent services have their own `/workspace` volume and no host mounts. To run a
package in a service (for example, the Minecraft client at `/workspace/castle`),
an agent copies `/workspace/.control-tools/mc263-client` into the service with the
generic `control_service_copy` tool, which accepts `.control-tools/<name>` as its
one Control-internal source. Control adds no package-specific path.

A read-only bind mount per execution adapter is not used: it would need changes
to each adapter's restart identity checks, and the copy already keeps the store
immutable.

## Security

- **No auto-install.** Control never runs package code, setup commands, Git
  hooks or filters on the host. Agents run setup inside their own containers or
  services, with the existing offline and resource limits.
- **Admission authority.** Only an authenticated local operator can import,
  admit or revoke. Workers, judges, rooms and Discord users cannot. The operator
  MCP import and revoke tools are hidden from the `control mcp` stdio bridge that
  external harnesses use.
- **File rules.** Admission validates the commit's tree with `git ls-tree`
  before any checkout, then checks the extracted files again:
  - Symlinks, submodules (gitlinks) and special files are rejected.
  - Names that project import would exclude are rejected, not silently dropped:
    environment files (`.env`, `.env.*`), credential files and directories (`.ssh`,
    `.aws`, `auth.json`, `credentials.json`, `.netrc`, private keys), Control and
    agent configuration (`.control-*`, `.claude`, `.codex`, `.mcp.json`) and
    generated directories (`node_modules`, `target`, `dist`, `result`, `.venv`).
    A package installs its dependencies with its `setup` commands.
  - Names must be UTF-8. Depth is at most 32.
  - A version holds at most 20,000 entries and 256 MiB.
  - The source folder cannot be the home directory, the filesystem root, or
    Control's configuration or state, as with project import.
- **No credentials.** The exclusion list is the same defence as project import.
  It is not universal secret detection. The operator reviews what they admit.
- **Judges** can search and read captured packages and see them in their
  read-only source. They cannot import or revoke.
- **Provenance** is written by Control from the admitting action, never taken
  from package files.

## Not yet supported

- Agent proposals and an operator review queue. Like knowledge proposals,
  Control would record the proposing actor, run and attempt itself, apply the
  import file rules, and let only the operator admit or reject.
- Signing, federation, remote registries, and publishing or fetching packages
  between nodes.
- Dependency resolution between packages, and running setup automatically.
- Per-package resource quotas beyond the size bounds.
- Garbage collection of revoked versions.
- Measured reuse: a fresh swarm reusing an admitted package, compared with a
  rebuild run.
