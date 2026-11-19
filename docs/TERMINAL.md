# Developing Control from the terminal

The TUI is a client of the same server the website uses. It has no separate agent
loop or terminal-only tools. Start the server, then run `control tui`. Use
`/improve REQUEST` to ask agents to develop Control; the node must already have
improvement authority, a source root, a provider and tool containers configured.
`/open /absolute/path` requests another project folder, `/trust` approves the
exact request shown and `/dismiss` rejects it. Agents work in isolated [workspaces](WORKSPACES.md).

## Composer

Type `/` for suggestions. Known commands and prefixes such as `/con` are shown in
purple. Use ↑/↓ to choose and Tab to complete; completion never sends or runs the
command, and makes no server or model call. Enter sends; Alt+Enter inserts a
newline. `/help` opens a scrollable reference of commands and shortcuts; Esc or
`/chat` returns to the conversation. Shared chat commands (`/improve`, `/spawn`,
`/answer`, `/open`, …) come from `crates/context/src/commands.rs`; terminal-only
actions such as `/activity`, `/lenses`, `/context`, `/trust`, `/dismiss`, `/port` and `/bot`
are added in `src/terminal/command_menu.rs`.

## Agent tools

Agents use the server's existing tools: workspace commands and background jobs,
subagents and messages, persistent services and watches, web search and fetch,
offline HTML screenshots, artifacts, knowledge proposals, and scoped draft-PR
publication. `rg`, Git, builds and tests run in the configured tool environment;
what is available depends on that image, not the host.

## GitHub

`/github` works on the repository configured for [GitHub publication](GITHUB.md)
and makes no model call or chat post:

```text
/github PR [overview|files|comments|reviews|review-comments|checks|statuses] [PAGE]
/github PR comment|title|body TEXT
/github PR merge HEAD_SHA [squash|merge|rebase]
```

Merge requires the exact head SHA and is available only when the operator's
GitHub binding was configured with `--allow-merge`. Checks and legacy statuses are
separate views of the captured PR head; an empty result does not mean CI passed.

## Not yet available

An end-to-end terminal development loop still needs:

- **Review and permissions:** terminal views for workspace diffs, artifact files,
  tool schemas, active jobs and services, and every pending request type
  (including browser origins). Folder, port, bot and design-question (`/answer`)
  decisions already have commands.
- **GitHub follow-up:** full diff and workflow-log inspection, and pushing new
  revisions to an existing PR after review.
- **Interactive execution and browser checks:** durable interactive process
  handles and bounded browser navigation/input against approved previews.
- **Development environments:** reproducible dependency, cache and network grants,
  resource shapes, and build/test/profile artifacts, delivered as tool adapters
  rather than a privileged shell in the supervisor.
- **Composer editing:** cursor/word editing, paste handling, draft history and
  argument completion for chats, lenses and request IDs.

These belong in tools, configuration and client views over the existing
interfaces. Secrets stay outside prompts and shell environments. The acceptance
test is an isolated terminal-driven edit, build, test, review and draft-PR cycle,
with a durable approval wait, restart and failure recovery, and no website.
