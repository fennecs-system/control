# Chat MCP

`control mcp --chat` lets an external agent drive Control's chats the way the
operator does. That agent might be Claude Code or the Codex CLI. With it, and
with the TUI or Discord for decisions, the whole operator loop runs without a
browser:

1. Open a DM.
2. Set its model and genes.
3. Request folders.
4. Send the prompt.
5. Watch progress.
6. See pending requests.
7. Cancel the run.
8. Stop its services.

It is a sibling of the read-only bridge (`control mcp`, see [Claude](CLAUDE.md)).
It uses the same stdio transport and the same operator connection, but serves
only chat tools.

## Register it with Claude Code

Start Control (`control serve`, then `control open` once), then:

```sh
claude mcp add --transport stdio control-chat -- /absolute/path/to/control mcp --chat
```

Reload Claude Code. The tools appear as `mcp__control-chat__chat_send` and so on.
Set `CONTROL_ADDR` in the server's environment if it doesn't listen on the
default `127.0.0.1:8080`.

## Tools

Read tools have `readOnlyHint: true`.

| Tool | Kind | What it does |
| --- | --- | --- |
| `chats_list` | read | Lists chats, most recent first. Shows each chat's status, mode, parent and any pending folder request. Takes `limit` and `roots_only`. |
| `chat_read` | read | Shows recent messages, replies and the latest activity events of each turn, as plain text. Page back with `before`. Set the page size with `limit` (at most 20), the events per turn with `activity` and the clipping with `max_chars`. A result is capped at 48 KiB. |
| `chat_team` | read | Lists the chat and every agent chat spawned from it, with status, plus the live services they own. |
| `chat_requests` | read | Lists everything the task waits on the operator for: design questions, bot and port requests, notices and folder requests. Each line has its ID and the chat to decide it in. |
| `chat_create` | write | Creates a DM. Takes an optional `title`, `model` (as `/model` accepts), `genes` (lens IDs) and `persona`. Returns the chat ID. |
| `chat_send` | write | Sends a plain-text prompt as the local operator. |
| `chat_model` | write | Selects the model for future turns (`/model`). With no model, it lists the choices. |
| `chat_genes` | write | Sets the genes for future messages. An empty list clears them. |
| `chat_open` | write | Requests host folders, as an agent's folder request does. The request waits for the operator. |
| `chat_cancel` | write | Cancels the current turn, or `run_id`. With `all: true` it cancels the whole task tree and its bots (`/cancel all`). |
| `chat_stop_services` | write | Stops the chat's live services. `team: true` includes its spawned chats; `service_id` stops one. |
| `chat_decide` | write, opt-in | Decides a listed request, for the kinds enabled in Manage → Permissions. Only listed when at least one kind is enabled. |

A typical run:

1. `chat_create {title, model, genes}`
2. `chat_open {id, paths}`
3. `chat_send {id, message}`
4. Call `chat_read` / `chat_team` until the run is done, and `chat_requests` when it waits.
5. `chat_cancel {id, all: true}` and `chat_stop_services {id, team: true}`

A chat holds one pending folder request at a time. Without folder approvals,
`chat_open` takes one path, and the operator approves it before the next. With
folder approvals enabled, `chat_open` requests and approves each path in turn.

## How it works

Every call goes to the running server's operator MCP endpoint (`/mcp`) over
loopback, through the `Hive` client the TUI uses. The server runs its own
application functions under its state lock:

- `control.chat.create`, `.send`, `.read`, `.lenses`, `.rename` and `.persona`;
- `control.chats.list` and `control.requests.list`;
- `control.project.request`;
- `control.services.list` and `control.services.stop`.

`control.services.stop` makes the same stop transition as the services page.

Folder and port decisions use the same confirmed forms as the TUI. Bot and
question decisions are the chat's own `/bot allow|deny` and `/answer` commands.
Model selection and cancellation are `/model` and `/cancel`. The bridge builds
these commands from validated single-word arguments.

The bridge never opens state files, so it is safe to use alongside the web UI,
TUI and Discord.

## Requests and notifications

An MCP caller may *raise* folder requests (`chat_open`). Its runs' agents raise
port, bot and folder requests and design questions as usual. Every kind appears
in `control.requests.list`, and so in the same places:

- `chat_requests`;
- Manage → Permissions;
- the Discord adapter's notifications.

The Discord notifications cover questions, bots, ports, notices and folder
requests. Turn on forwarding in Manage → Integrations to get them.

You answer questions, bots and ports by Discord reply. Folder requests open the
DM instead: approve them in Control, where the exact path is shown. Discord
settings saved before folder requests were forwarded still forward them along
with ports.

## Safety model

**Who it acts as.** The bridge authenticates with the node's local operator
credential (the private `operator-session` file, or `CONTROL_OPERATOR_FILE`), the
same way as the TUI and `control mcp`. Anything the operator starts on this host
with read access to that file acts as the operator. Only register it in a
harness you'd let type into your DMs. The credential is sent only as a loopback
cookie. It never appears in tool input or output, URLs or logs. The server
accepts only loopback HTTP origins.

**Decisions are off by default.** By default the bridge can't approve or deny
anything:

- folder access (`/open` grants host paths);
- ports;
- bots;
- design questions.

`chat_send` refuses these before contacting the server:

- any message that starts with `/` (so no `/open`, `/bot`, `/answer`,
  `/improve`, `/spawn`, room or memory commands);
- chats in improve mode;
- chats waiting on a design question, because plain text would answer it.

**Manage → Permissions** (`permissions.json` in the node configuration
directory, outside the checkout) can let the bridge decide some kinds,
one switch per kind:

- folder;
- port;
- bot;
- question.

The bridge rereads the file on every call.

The risk is that an agent with a kind enabled can approve its own run's
requests. That includes opening host folders to a task, if folder is enabled.
This switch is a policy for the bridge, not a security boundary: anything that
holds the operator credential could post the same forms itself.

The same page lists every pending request with a link to its DM.

**What it can always do.** It can start model turns, which use the operator's
model quota. It can cancel runs and stop services in any operator chat. These
are the operator's own powers in a DM, and need no approval in the UI either.

## Control's own agents

Manage → Permissions also has a switch, off by default, that lets Control's
agents use the chat tools. An example is driving a test DM in an improvement
loop. When it's on, workers get one tool, `control_chat {tool, arguments}`. It
runs the same chat tools (`src/mcp_chat.rs`) in process, scoped to the agent's
own task:

- It reads only its own chat and the chats spawned below it.
- It writes only to those descendants, never to its own chat or its parents.
  `chat_create` makes a child of its chat, up to 32 chats per task.
- It sends only plain text, `/model` and `/cancel`.
- It never gets `chat_open` or `chat_decide`, whatever the bridge's settings.
  Agents keep `control_project_request` for folders.
- Judges can't use it.

When the switch is off, the tool isn't listed and calls are refused.

Agents never receive the operator credential. The tool runs inside the server
and checks the calling attempt like other worker tools.
