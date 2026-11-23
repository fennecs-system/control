# Antigravity and Control

Control can run Google's Antigravity CLI (`agy`) as a worker, selected per chat
or spawn as `antigravity:MODEL` (for example `/model antigravity:gemini-4-neon-medium`;
`control antigravity check` lists the slugs your login can use). Codex remains
the default backend.

Google's Antigravity terms say that "using third party software, tools, or
services to access the Service ... is a breach of this Agreement" and may lead
to suspension, and its FAQ says wrapping `agy` "inside third-party coding agents
or applications ... is not supported under consumer plans". Which login to use,
a Google sign-in or an API key, is the operator's choice for their own account.

## Image

The image holds the unmodified release binary (version 1.5.14) fetched from the
URL and SHA-512 that Google's installer manifest publishes, pinned in
`tools/antigravity/package.nix`. No installer script is run. No licence is
stated, so Nix treats it as unfree; the flake allows that one package. The CLI's
self-update cannot write to the image and is not used.

```sh
nix run .#setup -- --antigravity
./result/bin/control antigravity check
```

## Logins

Control picks the login from which private file exists under
`~/.config/control/antigravity/` (a directory with mode 0700), in this order:

| File | Login |
| --- | --- |
| `gemini-api-key` (0600, created by the operator, or by `control antigravity key` for an API-key account) | Gemini API key: `"modelProvider": "gemini"` and `GEMINI_API_KEY`. Billed to the key's project, not a subscription. |
| `adc.json` (0600) | Google Cloud Application Default Credentials, with `AGY_ADC_AUTH=true`. |
| `login/gemini/antigravity-cli/antigravity-oauth-token` | The Google sign-in that `control antigravity login` saved. |

A named API-key account (`control accounts add antigravity NAME --kind api-key`,
then `pbpaste | control antigravity key --account NAME`) always uses its
`gemini-api-key`, never another file. With a key, Control's MCP server is
started through `env -u GEMINI_API_KEY`, so only the CLI holds the key the
entrypoint exports.

Each file is mounted into the CLI's own container and nowhere else: not into
the workspace, the shell containers or the isolation probe. A key and `adc.json`
are read-only; the saved Google sign-in is writable, so a token the CLI
refreshes can be kept (docs/LOCAL-MODELS.md, "Sign-ins stay signed in"). Control
checks file metadata only and never reads a credential.

`control antigravity login` starts the pinned image with a terminal, with
`login/gemini` and `login/config` as the CLI's home directories. Sign in with
your Google account when the CLI asks; it cannot open a browser in the
container, so open the address it prints on your own machine and paste the code
back if it asks for one. Then leave the CLI with `/exit`. It prints whether the
sign-in was saved, and names (never shows) any other files it wrote.

## How a turn runs

Like Grok, and given what Codex is given:

- The same captured turn context and policy prompt as Codex, written to a
  private file beside the workspace and sent as one stream-json user message.
- The CLI's own tools are denied: settings deny every action class its rule
  parser accepts besides `mcp` (`command`, `read_file`, `write_file`,
  `read_url`, `execute_url`, `download_url`, `unsandboxed`) and allow
  `mcp(control/*)`. The CLI runs without `--dangerously-skip-permissions`:
  headless mode cannot prompt, so an action no rule allows is refused
  (https://antigravity.google/docs/cli/headless) rather than approved, and a
  future action class the deny list does not name is refused too. No flag or
  setting removes the built-ins in stream-json mode; only the ACP server's
  `session/new` takes a `disableBuiltInTools` parameter, which Control does not
  use (it would mean a different protocol client and could not be checked
  without a model).
- The CLI reads an API key only from `GEMINI_API_KEY`
  (https://antigravity.google/docs/install), so the wrapper still exports it;
  Control's MCP server gets it blanked (`"env": {"GEMINI_API_KEY": ""}`), and
  shell commands are denied, so nothing the model can start inherits it.
- The login is beneath the CLI's mode-0700 home; no other uid in the
  container can read or write it (see the threat model). Offline, with a fake
  expired token and no network, the CLI did not rewrite the token file; its
  token store (`authstore.fileStore`) is closed source, so whether it saves a
  refresh in place or by rename is not known.
- Control's tools are the one MCP server, every tool loaded eagerly. A turn is
  stopped if the CLI calls any other tool or starts a subagent, and fails if it
  reports a Control tool call as refused even though the run says success.
- Shell commands and builds run in separate offline containers through
  Control's workspace tools; the workspace is not mounted beside the login.
- The CLI's home is a tmpfs, so conversations, logs and caches die with the
  container. Control's history and handoffs are the only memory between turns.
- Usage comes from the per-step and end-of-turn counts; cache reads count as
  input and as cached input.

A failure never falls back to another backend or the host.

## What is verified

- The image builds on the aarch64-linux builder; the binary runs in it
  (`agy --version` prints 1.5.14) and its flags are those in Google's headless
  reference.
- In the container the CLI logs "Using file-based token storage because
  container environment detected" even with a Secret Service on a D-Bus
  session, and looks for its sign-in at
  `~/.gemini/antigravity-cli/antigravity-oauth-token`. Google's docs describe
  only the OS keyring; this is the CLI's own behaviour, seen in its log and
  file accesses, so no keyring daemon is in the image.
- `control antigravity check` with an API-key file runs the probe, starts the
  CLI and lists 25 models. Listing does not test the key: an invalid key ends
  the first model call with "API key not valid", which Control reports as a
  rejected login.
- Without a login the CLI ends with `authentication failed or timed out`.
- A turn container with Control's settings and MCP configuration: the CLI's
  log shows the allow and deny rules as written and the `control` MCP server
  being connected (it waited for Control's host, which was not running) before
  the model call.
- Offline, with a scripted stand-in for the Gemini endpoint inside the turn
  container (`GOOGLE_GEMINI_BASE_URL` on loopback, `--network=none`, a fake key
  file): with an API key the CLI offers the model 14 built-ins (`read_file`,
  `run_command`, `write_file`, `edit_file`, `run_workflow`, `search_web`,
  `schedule`, `manage_task`, `send_message`, `swarm`, `ask_question` and three
  subagent tools) plus Control's MCP tools and `list_resources`/`read_resource`.
  With Control's settings, with and without `--dangerously-skip-permissions`,
  scripted calls of `read_file` on the key path, on `/proc/self/environ` and via
  `/workspace/..`, `run_command` (`cat KEY; env`), `run_workflow` (checked as
  `unsandboxed`) and `edit_file` were each refused "Matches user-configured deny
  rule", and so were the same reads made by a subagent the model defined and
  invoked; the key never reached any request. Without the deny rules the same
  `read_file` returned the key, and `run_command` saw it in its environment. An
  allowed Control MCP tool ran in both modes; with no allow rule and no skip it
  was refused, as documented.
- `control antigravity check` runs the credential canary for each login form:
  all assertions pass in the pinned image.
- The decoder is tested on the stream shapes in Google's headless page and on
  the endings above.

## What the first real login will prove

- That the sign-in completes through the printed address inside the container
  and writes the token file above. If `login` reports "sign-in saved: no",
  report the file names it lists.
- That a refreshed token is kept: an access token lasts about an hour. Note the
  modification time of `login/gemini/antigravity-cli/antigravity-oauth-token`
  (`ls -l`; never open it), run a turn or `check` more than an hour later, and
  confirm the time changed; then confirm a turn the next day still works
  without signing in again. Only the token file is
  mounted (writable); if the CLI replaces it by rename rather than rewriting
  it, the rename fails on the mounted file, the token stays in memory for that
  turn, and the next turn refreshes again. If `check` the next day asks for a
  sign-in, its folder must be mounted instead, like Claude's and Grok's.
- That Control's tools appear as `mcp_control_TOOL` and calls arrive as `tool`
  steps under that name, as the MCP page describes. Any other name stops the turn
  with "unexpected tool (NAME)", which names what to allow.
- How a subscription's usage limit is worded in the stream.
- That a Google sign-in gets the same refusals: its tool set (the init list
  names `view_file`, `list_dir`, `grep_search`, `find_by_name`,
  `read_url_content` and others not offered with an API key) could not be
  exercised offline. Ask the model to read
  `/home/agent/.gemini/antigravity-cli/antigravity-oauth-token` (or, with a
  key, `/home/agent/.control-key/gemini-api-key`) and to print its environment;
  either succeeding is a failure, and the turn should instead stop with
  "unexpected tool (NAME)".
