# Developing Control from the terminal

The TUI is a client of the same server used by the website. It does not need a
second agent loop or terminal-specific tools. Start the server and connect with
`control tui`; use `/improve REQUEST` to develop Control. Improvement authority,
the source root, provider and tool containers must already be configured on the
node. `/open /absolute/path` requests another project folder and `/trust`
approves the exact displayed request; agents work in isolated workspaces.

Type `/` for command suggestions. Recognized commands and prefixes such as
`/con` are purple. Use ↑/↓ to select and Tab to complete; completion never sends
or executes the command. Enter sends the completed draft. `/help` opens a
scrollable command reference; Esc or `/chat` returns to the conversation.
Shared chat commands come from the existing command definitions, with local
terminal actions added in the client. Completion makes no server/model calls.

Existing agent tools cover workspace command execution and background jobs,
subagents and messages, persistent services and watches, web search/fetch,
offline HTML screenshots, artifacts, knowledge proposals, and scoped draft-PR
publication for completed improvements. `rg`, Git, builds and tests can run in
the configured tool environment; their presence and dependencies are environment
capabilities, not guaranteed host tools. See [GitHub publication](GITHUB.md) for
the clean-baseline, configured-repository and text-output limits. `/github PR`
reads that repository's PR overview; add `files`, `comments`, `reviews`,
`review-comments`, `checks` or `statuses` and an optional page number for bounded
evidence. This makes no model call or chat post. Checks and legacy statuses are
separate views of the captured PR head, with no claim that empty results pass CI.

The remaining work for an end-to-end terminal development loop is:

- **Review and permissions:** terminal views for workspace diffs, artifact files,
  tool schemas, active jobs/services and all pending requests, including browser
  origins. Reuse typed server queries and existing approval operations. Folder,
  port, bot and design-question decisions already have terminal commands; they
  do not establish parity with every web management view.
- **GitHub follow-up:** complete diff/log inspection, explicit comment writes and
  updating a PR after feedback. The current publisher opens a draft from a completed
  checkpoint; it is not a general GitHub integration. Publication does not merge
  or deploy, and the terminal should keep those decisions explicit.
- **Interactive execution and browser verification:** durable interactive
  process handles and bounded browser navigation/input against approved
  previews. Background commands and offline screenshots exist; a screenshot
  alone cannot exercise the running app's interactions.
- **Development environments:** reproducible dependency/cache/network grants,
  resource shapes, and build/test/profile artifacts. Use service/tool adapters
  and the existing resource roadmap, not a privileged shell in the supervisor.
- **Composer ergonomics:** cursor/word editing, paste handling, draft/history
  navigation and argument completion for known chats, lenses and request IDs.
  Command-name completion is the first increment, not a full terminal editor.

These belong in tools/configuration and client projections over the application
interfaces. Keep secrets in the planned Vault broker, outside prompts and shell
environments. Acceptance should run an isolated terminal-driven edit, build,
test, review and draft-PR cycle with a durable approval wait, restart and
failure recovery; no website should be required for that supported workflow.
