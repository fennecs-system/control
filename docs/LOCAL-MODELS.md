# Local models and history compaction

Control calls configured text endpoints; inference runs in a separate service or
container. Control does not embed llama.cpp or download weights. A configured
local endpoint can:

- summarize chat history for Codex, Claude and local chats (the compactor);
- answer bounded text requests (`control_model_generate`);
- run agent turns through Control's native tool loop, if it serves tool calls.

`nix run .#inference` starts one llama.cpp service per model with the platform's
accelerated backend.

## Selecting a model for a chat

In chat and the TUI, `/model` lists the configured agent models (text-only local
models are listed separately, with how to enable them), `/model PROVIDER:ID`
selects the model for future turns, and `/model default` returns to the node
default. The selection is chat metadata: it starts no model traffic, does not
interrupt an active attempt and is excluded from model history. Add models in
Manage first. Account access is checked by the adapter when a run starts, not
inferred from the name.

Both spawn tools accept an optional `model`. Omitting it inherits the parent's
captured provider and model; an explicit selection is captured with the child and
keeps its tool permissions and role. Invalid selections are rejected before any
effect, and changing a retried request's selection is a conflict. There is never
a silent fallback to another provider.

## Configure text models

In **Manage → Models & providers → Text models**, add one entry per model:

- a unique name and the model alias the endpoint serves (one entry per endpoint
  and model ID);
- a loopback or private literal address ending in `/v1` (or `localhost`);
- a weights/runtime revision, ideally the GGUF SHA256 plus the runtime revision;
- declared context and output token limits, input and output byte ceilings and a
  per-request timeout (the form suggests 1800 s). There is no retry or hosted
  fallback;
- **Serves tool calls** when the server runs with tool calls (`--jinja`), so the
  entry can also run agent turns.

Each entry lists its capabilities (summaries and text generation, plus agent
turns when it serves tool calls). Roles refer to entries by name: the compactor
under **Memory Tree model**, `control_model_generate` by `model`, and agent turns
by `/model local:NAME`. **Edit** changes everything except the name and adapter,
with the same validation as adding. Older settings that had two entries for the
same endpoint and model are merged on start; the tool-capable entry's name is
kept and selections follow it.

No credential is forwarded to an endpoint; authenticated endpoints are not
supported. These settings live in the private `model.json` in the configuration
directory.

`control models test MODEL_ID` sends one small request and reports response size
and usage, without history or setting changes.

## Memory Tree model

Compaction is opt-in and off by default. Choose a compactor, list the exact chat
IDs to process and enable **Automatic summaries**; an empty list processes
nothing. A chat whose model is a local endpoint is compacted by that endpoint by
default (see [local agent turns](#local-agent-turns)). Compaction changes only
what enters each fresh turn; a long Codex or Claude attempt can still run that
CLI's own private compaction.

In a DM (web composer, TUI, or Discord as `!memory`), `/memory` (alias
`/compact`) shows whether the Memory Tree is on, which compactor serves it and
why (operator opt-in, local chat model default, opted out or off), how many
entries are verbatim or folded, and the `/dm/ID/memory` link. The operator
(local operator or a Discord architect) changes it per DM through the same
settings save as the Models page: `/memory on [MODEL]` lists the DM and turns
automatic summaries on, using MODEL (a configured text model), else the DM's
local endpoint, else the configured compactor. The compactor is shared by every
listed DM, so changing it while others are listed requires naming MODEL.
`/memory off` unlists the DM and records an opt-out that also overrides the
local-model default; listing it again (command or Models page) clears it.

The compactor can also be the Codex or Claude CLI adapter (leave the URL empty).
It reuses the pinned worker image and login isolation, with no project mount and
no Control tools: Claude's advertised tools are checked against an empty list,
and Codex's execution features are disabled, so any tool use rejects the result.
CLI output is byte-bounded; its output-token setting is an input reserve, not an
in-flight token cap. A failed call keeps private ownership records; recovery
stops only a verified matching container whose owning host PID is gone.

### How it works

The newest entries stay verbatim. Older aligned ranges become tree leaves, and
larger ranges combine two child summaries, so no request needs the whole chat.
Leaves split further when their text exceeds the compactor's input budget. A
single oversized entry or child pair fails visibly; nothing is truncated. The
budget reserves the declared output plus 512 tokens and counts each input byte as
a token. This is conservative, not a tokenizer, and a server may still reject a
request.

Every summary node, of chat history or of a local turn's steps, uses one
structured format: short bullets under **Goal & constraints**, **Findings**
(facts and diagnoses with their exact paths, names and values), **Errors &
fixes**, **Decisions/plan**, **Open questions** and **Next step**. A parent built
from two child summaries merges them section by section, keeping every finding
and decision unless later evidence replaces it, so findings survive any number of
levels instead of being flattened into a list of actions. Changing the prompt
changes summary identity, so existing chat summaries are rebuilt once.

Summary nodes are private atomic files under `summaries/` in the state directory.
Their identity covers the chat, source hashes, settings, prompt and declared
model revision, so a changed source or model never reuses a stale summary.
Completed nodes survive restarts and are reused. Original history stays
authoritative. Summaries are untrusted, lossy evidence; the original assignment,
current request and committed working state stay outside them.

The worker runs one job at a time. It takes a bounded snapshot under the state
lock and does disk and network work outside it, rechecking authorization and
configuration before each call and publication. Paused or stopping bots are
skipped. Disabling summaries stops new calls after an in-flight request settles;
it neither cancels that request nor deletes cached results. A failed job does not
retry the same chat revision. Missing or invalid summaries fall back to recent raw
entries within the byte budget, with failures shown in the inspector.

Source is read in pages of at most 64 entries and 1 MiB, and at most 16 ready
summary views are kept. There is no total history ceiling, but source descriptors
and history indexes grow with history, and there is no disk cache retention or
garbage collection yet. "Infinite" chat is not free storage.

### Capacity

Advanced settings set the number of recent verbatim entries, the maximum leaf
size (a power of two) and the rendered history byte budget. Size the history
budget for the chat model, leaving room for its request, instructions and tools;
it is independent of the compactor's capacity. Each chat model can also declare
context and output capacity and a provider overhead reserve. When declared, the
input planner keeps all mandatory sections, adds optional evidence newest-first
within the remaining budget, and records every omission in the input trace.
Mandatory content that cannot fit blocks admission with a budget error; it is
never truncated. Without declared capacity, the provider's existing policy
applies.

### Inspect and recall

**Memory Tree** in a DM (`/dm/ID/memory`; the older `/dm/ID/compaction` and
`/chat/ID/compaction` redirect there), or `/compact` in the TUI, shows the history the next turn
starts from as a diagram: older ranges folded into aligned summary blocks
(level L covers 2^L entries) above the journal positions they cover, and the
newest entries verbatim. Each block's tooltip gives its range, level, token
and byte sizes, compactor model and time; legend marks pending blocks,
operator revisions, pins and dropped ranges. The overview is drawn from
memory only: O(log n) summary blocks plus at most 24 verbatim cells (older
verbatim entries share one cell), so its cost does not grow with history.
Selecting a block loads its detail lazily into a frame: its two children (or
leaf entries), summary text, versions, sizes and a link to the original source.
The detail reads one root-to-block path of node files outside the state lock.
Its **Compression** row scores the summary: how many times smaller it is than
its input, its character entropy in bits per character (padded or repetitive
prose scores low, terse fact lists high) and, for a summary of two summaries,
how many of their hard tokens (numbers, IDs, paths, coordinates) it kept. A
low kept share shows where a fold lost facts the agent may need.
Inspection makes no model calls.

Measured on an Apple M-series laptop (release build, 200 renders each, every
cut block summarised; `memory_tree_render_benchmark` in
`src/compaction/tree/tests.rs`, run with `--release -- --ignored --nocapture`):

| entries | cells drawn | page bytes | overview render |
|---|---|---|---|
| 100 | 17 | 15.5 KB | 65 µs |
| 1,000 | 21 | 19.1 KB | 69 µs |
| 10,000 | 25 | 22.2 KB | 85 µs |
| 100,000 | 30 | 27.1 KB | 99 µs |

A block detail page (summary, two children, versions; four node files read
outside the lock) takes about 83 µs.

### Editing the Memory Tree

The operator can edit what the agent remembers from the Memory Tree; agents
cannot (there is no agent tool or route, and `control_history_read` stays
read-only). Edits are saved with the DM and every change is logged.

- **Regenerate with emphasis.** Select a block, enter a focus ("keep the
  port/relay details") and **Regenerate summary**. The DM's compactor
  summarises the same inputs as before (a leaf's entries or the two child
  summaries) with the emphasis added to the summary instructions, which it
  never relaxes. The result is a new revision, selected at once; the automatic
  summary and every earlier revision stay listed and selectable (**Use this
  version**). Nothing is overwritten. A revision of a block inside a larger one
  changes that parent's cache key, so the next compaction rebuilds only its
  ancestors from the revision.
- **Pin / unpin.** A pinned entry (at most 32 per DM) stays verbatim in the
  next turn even when its block is summarised: it follows the summary that
  covers it. Pins also apply with the default recent-entries view.
- **Drop / restore.** A dropped range leaves the agent's view: its verbatim
  entries, retained instructions, the original assignment and recall's source
  reads. Summaries are rebuilt from a redaction marker in its place; until
  then the turn uses the byte-bounded recent view. A revision overlapping a
  dropped range is not used. **Restore** reverses it; the journal itself is
  never rewritten.

The agent's next turn uses the edited tree: the projection that builds every
turn (managed context, the legacy prompt and fork snapshots) applies the
selected revisions, pins and drops, and `control_history_read` returns the
selected revision with its emphasis and model.

Agents use `control_history_read`: omit `root` to list this conversation's roots,
then follow a `path` of 0/1 child indexes; `source=true` reads a leaf's original
text. Each read returns at most 16 KiB with a continuation offset, and only roots
owned by the current conversation can be read. Results carry checksums, ranges,
model revision and usage. Source text is untrusted and may describe old state.

The listing also returns `turns`: the step trees of this conversation's current
and recent local-model attempts (below). Their roots are read the same way, with
`start`/`end` counting the attempt's steps. `query` searches them, pi's
`session_history` style: every whitespace-separated term must appear
(ASCII case-insensitive), newest attempt and steps first, in node summaries and in
the original steps. Each hit is a verbatim excerpt of about 600 bytes with its
`root`, `path`, step and `offset`, so `{"root":…,"path":…,"source":true,"offset":…}`
reads on from it. A search reads at most 256 nodes and returns at most 8 hits
within `limit_bytes`; `complete: false` says it stopped early. It searches local
turns only, not the chat tree's own sources.

### Compare compactors

`control models compare MODEL_ID [MODEL_ID ...]` calls the models only when run.
It summarizes a synthetic fixture (no live chats or credentials) once flat and
once as a binary tree (seven calls), and reports exact identifier recall, missing
facts, elapsed time, usage and the raw summaries. The `retained-constraints-v2`
fixture in `tools/compaction-eval/cases.json` scores ten anchors; identifier
matching rejects longer IDs and child paths. Compare results only within one
fixture ID. Read the summaries too: quoting an injected canary while rejecting it
is not an injection failure.

## Direct text generation

Every **Local endpoint** model (not the CLI adapters) also serves bounded text
generation, from the same `model.json` entry, limits and revision.

- Agents call `control_model_generate`; the operator's `control mcp` bridge
  exposes it as `control.model.generate`.
- Arguments: `prompt`; optional `model` (may be omitted when exactly one endpoint
  model exists); optional `system` (at most 8 KiB); and optional
  `max_output_tokens`, which can only lower the declared ceiling.
- Shell: `control models generate MODEL_ID [--max-output-tokens N] < prompt.txt`,
  and `control models bench MODEL_ID [RUNS]` for sequential timed generations.

The result contains the text, usage (`usage_reported: false` and `null` usage
when the server sends none, never zeros), elapsed time, output tokens per second,
the applied limits and provenance: name, alias, declared revision, and the
server-reported model and `system_fingerprint`. Server-reported fields are
untrusted metadata.

The call sends text only: no tools, files, network or credentials, and the reply
is labelled untrusted. Input that exceeds the declared context is refused before
connecting. A reply cut at the output limit, a tool call, an empty or oversized
reply, or malformed JSON is an error; partial text is never returned. One
generation runs at a time per Control process; a concurrent call is refused, not
queued. The compactor uses the same endpoint independently, so size the server's
slots (`-np`) for both.

The call runs in the Control host process, so workers need no network route to
the endpoint. Over plain HTTP, cancelling the run or stopping Control closes the
connection, which stops llama.cpp generating. HTTPS calls check cancellation only
before sending; otherwise the declared timeout bounds them.

## Local inference service

`nix run .#inference -- --model /absolute/model.gguf` starts a llama.cpp server for
one model. Each `--alias` is its own service, so several models can run on
separate ports. The binary comes from the pinned nixpkgs, weights are never
downloaded, and the server binds to loopback only.

| Backend | Selected when | What runs |
| --- | --- | --- |
| `metal` | default on Apple Silicon macOS | the pinned darwin `llama-server` with `-ngl 999`, as launchd agent `dev.control.inference.ALIAS`, logging to `~/.local/share/control/inference/ALIAS.log` |
| `container` | default elsewhere | the pinned Linux image as container `control-inference-ALIAS` (read-only, all capabilities dropped, model directory mounted read-only); CUDA (`--gpus all`) when `nvidia-smi` works and Docker has the NVIDIA runtime, otherwise CPU |
| `cpu` | explicit | the CPU container on any platform |

Llama.cpp runs natively on Apple Silicon because a container cannot reach the GPU
there. It is the only host-native third-party service, pinned by `flake.lock` and
`tools/inference/llama.nix` (an overlay ahead of the nixpkgs release, for models
whose support landed after it),
with no Python or Node.

| Flag | Default |
| --- | --- |
| `--backend metal\|container\|cpu` (or `CONTROL_INFERENCE_BACKEND`) | auto, then the model's saved backend |
| `--acceleration cpu\|vulkan\|cuda` | auto (`vulkan` passes `/dev/dri`; CUDA is unfree and needs the NVIDIA container toolkit) |
| `--alias NAME` | `qwen-local` |
| `--ctx-size N` | 32768 |
| `--port N` | 8081 |
| `--parallel N` | 1 slot |
| `--sha256 HEX` | hash the GGUF (reused while path and size are unchanged) |
| `--dry-run` | print the launchd plist or container command and change nothing |
| `--status`, `--stop` | every model, or one with `--alias` |
| `-- ...` | extra `llama-server` flags |

The CUDA and Vulkan image variants evaluate but have not been run. A start refuses
a port held by another model's record. Every start passes `--jinja` (tool calls
through the chat template), `--reasoning-format deepseek` (thinking arrives as
`reasoning_content`, apart from the answer) and `--metrics --slots`. There is no
server-wide reasoning budget: Control sets thinking per request (see
[Thinking](#thinking)). Services started by earlier releases ran with
`--reasoning-budget 0`; restart the service (run the start command again)
between turns to pick up the new flags. The
script writes `inference/ALIAS.json` in the configuration directory (backend,
model path, SHA256, alias, port and a revision string) and prints the values for
**Manage → Models & providers → + New text model**: adapter *Local endpoint*, the
alias as name and model ID, `http://127.0.0.1:PORT/v1`, the revision and context
size. It never edits `model.json`. A single-service install from an earlier
release (record `inference.json`) is stopped when a start reuses its alias or
port, and by `--stop`.

Unified memory is shared with Control, the container VM and other services: keep
weights plus KV cache well below physical RAM. One slot serializes agent steps and
background summaries; more slots split `--ctx-size` between them.

Running the same start command again restarts that model with the current flags
(pass the same `--alias`, `--port`, `--ctx-size` and `--parallel`). A restart
drops the KV cache and fails any in-flight local turn, which then leaves a host
checkpoint like any other failure.

### Several agents on one server

Agents sharing one slot take turns in it: each request evicts the previous
agent's prefix from the slot, and llama-server restores the best match from its
host prompt cache (`--cache-ram`, in MiB; 8192 by default). When that cache is
too small for every active agent's context, an agent's history is prefilled
again on each step. Size it as about 165 KB per cached token (observed for a 27B
Q4 model) × peak context × active agents: three agents at 24k tokens need about
12 GB. Pass it, like any extra `llama-server` flag, after `--`:

```sh
nix run .#inference -- --model /absolute/model.gguf -- --cache-ram 12288
```

`--parallel N` gives N slots, so agents run concurrently and each keeps its own
prefix, but the slots split `--ctx-size`: pass N × the per-agent context (for
example `--parallel 3 --ctx-size 98304` for 32k each), and declare the per-slot
value (each slot's `n_ctx` in `/slots`) as the model entry's context, since that
is what one request can use. Either way keep weights, KV cache and the prompt
cache well below physical memory. `requests_deferred` in the statistics line
counts requests waiting for a slot.

### Server statistics

`control models stats NAME [--json]` reads a local endpoint's server once:

```text
qwen-local: prompt 162.4 tok/s · generation 15.1 tok/s (avg since start) · 1 processing, 0 queued · prefix cache 91% of prompt tokens · prefilled 514.0k tokens, 45% of busy time · slots 1/1 busy · KV 26.9k/32.8k tokens (82%)
```

The same line appears on each local endpoint's card in Manage, refreshed from
`/providers/live` every 5 seconds while the page is open and visible; nothing is
polled otherwise or stored. Values come from llama-server's `/metrics` (needs
`--metrics`) and `/slots`, each read with a 2-second timeout and a 128 KiB
response limit; at most 1,024 metric lines are parsed.

- Tokens per second: `prompt_tokens_total / prompt_seconds_total` and
  `tokens_predicted_total / tokens_predicted_seconds_total`, averaged since start.
- Requests: `requests_processing` and `requests_deferred`.
- Prefix cache: `prompt_tokens_cached_total` as a share of prompt tokens.
- Prefill: `prompt_tokens_total` (prompt tokens evaluated, cache hits excluded)
  and `prompt_seconds_total` as a share of prompt plus generation seconds. A high
  share with a busy slot means prompts are being re-processed rather than reused;
  each local turn's `local_model` activity also gives its input tokens and how
  many were cached.
- Slots and KV: each slot's prompt plus generated tokens against its `n_ctx`.
  This approximates KV use; llama.cpp exposes no KV usage metric.

A server without `--metrics` shows `metrics off`; restart it with the current
script to enable them.

### Two models

Run one service per model, add one text-model entry each, then assign roles. For
example, a 27B model for agents and a 7B model for summaries:

```sh
nix run .#inference -- --model ~/.local/share/control/models/Qwen3.8-27B-Q4_K_M.gguf \
  --alias qwen-27b --port 8081
nix run .#inference -- --model ~/.local/share/control/models/your-7b-Q4_K_M.gguf \
  --alias qwen-7b --port 8082 --ctx-size 16384
nix run .#inference -- --status
```

1. Add `qwen-27b` (`http://127.0.0.1:8081/v1`) with **Serves tool calls**, and
   `qwen-7b` (`http://127.0.0.1:8082/v1`) without it.
2. Set **Memory Tree model → Summary model** to `qwen-7b`.
3. Use `/model local:qwen-27b` in a chat, or `model: "local:qwen-27b"` when
   spawning.

A chat on `local:qwen-27b` compacts on `qwen-27b` unless the operator lists it
under automatic summaries, which then uses `qwen-7b`, or opts it out with
`/memory off`. Budget memory for both
models' weights and KV caches.

### Manual container

The CPU image (`tools/inference/image.nix`) can also be run by hand. On macOS,
build the Linux output with a Linux builder, for example
`nix build .#packages.aarch64-linux.inference-image`.

```sh
nix build .#inference-image -o /tmp/control-inference-image
docker load -i /tmp/control-inference-image
docker run --rm --name control-compactor --read-only \
  --cap-drop ALL --security-opt no-new-privileges \
  --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --cpus 4 --memory 24g --memory-swap 24g \
  -p 127.0.0.1:8081:8081 \
  --mount type=bind,src=/absolute/operator/models,dst=/models,readonly \
  control-inference:nix \
  --host 0.0.0.0 --port 8081 --no-webui \
  --model /models/model.gguf --alias qwen-compactor \
  --ctx-size 32768 --threads 4 --reasoning-budget 0
```

Control asks for summaries and generation without thinking on every request, so
`--reasoning-budget 0` here only covers other clients. Resource limits are
operator choices. Any private OpenAI-compatible
`/v1/chat/completions` server also works; see the
[llama.cpp server documentation](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md).

## Run a Qwen 27B-class GGUF

1. Choose weights before downloading. The
   [Qwen3.8-27B GGUF](https://huggingface.co/ggml-org/Qwen3.8-27B-GGUF/tree/main)
   at Q4_K_M is about 19.4 GB; Q5_K_M and Q6_K need roughly 23 and 26 GB. A
   32768-token context added about 5.4 GB on Metal. Verify the published SHA256
   and keep weights outside the checkout, for example in
   `~/.local/share/control/models/`.
2. `nix run .#inference -- --model ~/.local/share/control/models/Qwen3.8-27B-Q4_K_M.gguf`
3. Add the endpoint in Manage with the printed values and **Serves tool calls**.
   Keep a generous request timeout: each step re-sends the core tool schemas
   (about 3.4k tokens). 1800 s is fine on Metal; CPU needs far more.
4. Check it: `control models test qwen-local`, `control models bench qwen-local 5`
   and `control models stats qwen-local`.
5. In a chat, `/model local:qwen-local`; `/model default` switches back.

Without **Serves tool calls** the entry is text-only: generation and summaries
work, and `local:NAME` is refused for agent turns. Tick it later with **Edit**.

## Local agent turns

A local endpoint with **Serves tool calls** can drive chat turns, spawned agents
and bot callbacks: `/model local:NAME` in a chat, or `model: "local:NAME"` in
`control_agent_spawn` / `control_agent_spawn_blocking`. A text-only endpoint is
refused before any effect. A local model is never the saved node default, and no
hosted model is substituted when the endpoint fails.

**Loop.** Control runs the loop (`src/local_agent.rs`); the endpoint only
generates. Each step is one bounded `/v1/chat/completions` request with a system
note, the same turn input Codex receives, and the attempt's worker MCP tools as
function tools: the role's core set plus `control_workspace_run`, the
credential-free offline shell Claude Code also uses. Other permitted tools are
named in the prompt's deferred index ([LENSES.md](LENSES.md#current-boundaries)).
Schemas returned by `control_tool_search` become function tools for the rest of
the turn, and a call to an indexed but unloaded name first fetches its schema
(without a model step). Unknown names never execute. Local turns do not get
`control_call`. Every call goes through the same worker dispatch as the CLI
mailboxes, so authorization, attempt and handoff fencing, judge policy and room
idempotency are identical. Calls run one at a time in the order emitted, at most
eight per step. A text reply without tool calls is the final report; a committed
`control_turn_handoff` ends the attempt. Workspace changes are checkpointed and
published as for Codex.

**Malformed output.** Arguments that are not a JSON object, unknown tools, more
than eight calls, an empty reply or a reply cut at the output limit never execute.
The model gets a corrective result and may retry; four malformed steps in a row
fail the attempt, and a valid step resets the count. A response without exactly
one choice fails immediately.

**Step cap.** A turn stops after `local_agent_steps` model steps (`execution.json`,
default 64, range 16–1024). Each step re-sends the turn's context, so the cap
bounds cost; the turn time limit is the main bound. Once per turn, when 8 steps
remain (a quarter of the cap if fewer), Control appends a note to the newest tool
result asking the model to publish a checkpoint with `control_turn_handoff`. A
turn that reaches the cap fails with cause `step_limit` and leaves the same host
checkpoint as a timeout (last tool calls, staged draft, captured workspace) for
the next turn. A persistent bot gets a recovery turn within
`bot_recovery_continues`.

**Cancellation.** The loop checks cancellation, design questions, blocking
delegation and the deadline before every step and after every tool call.
Cancelling closes the in-flight HTTP connection. Attempt-owned tool containers
stop when the loop ends; persistent services are separate. Recovery starts a
fresh attempt from Control's journal; server KV caches only speed up repeated
prefixes.

**Turn limit.** Local steps are roughly ten times slower than hosted ones, so
every local attempt (chat, spawn or bot) gets its own limit: 3600 s by default,
set with `local-turn-timeout-secs` in the configuration directory or
`CONTROL_LOCAL_TURN_TIMEOUT_SECS` (60 s–24 h; `0` or `none` for unlimited). It is
captured at admission and replaces the 600 s worker default and the 1200 s bot
default; an explicit `worker-timeout-secs` still wins. The deadline warning fires
when 15% of the limit (at least 90 s) remains, and a turn stopped at the limit leaves a host
checkpoint (`continuity.interrupted`, see [TURN-CONTINUITY.md](TURN-CONTINUITY.md)).
The model entry's request timeout bounds a single step.

**Context fit.** The endpoint's declared limits are the budget. Turn-input
planning reserves 7168 tokens (`OVERHEAD_TOKENS`: 4096 for core schemas, 2048
for schemas loaded during the turn, 1024 for the first exchange) and selects
history newest-first within the rest, recording omissions. Before each request
the loop estimates tokens at 3 bytes per token for messages plus offered schemas.
Input that cannot fit at admission blocks the run before any model call.

**In-turn context.** Each request is rendered from the turn's steps through the
same lens as chat history (`control_comonad::cut`, per step): recent steps
verbatim, older steps in aligned power-of-two blocks that never change once
complete. A result that repeats a long argument of its own call cites it instead.

While a request is below half of the input budget (`SHRINK_PERCENT`, 50%), every
step stays whole: each request is the previous one plus the new step, byte for
byte, so the server's prefix cache reuses all of it. When a request reaches that
share, every step but the newest four shrinks at once, without a model call:
tool results keep a 600-byte head and a size marker, and long string arguments
keep a 160-byte head while staying valid JSON. The shrunk steps then stay fixed
while the request grows back towards the threshold; the next shrink covers at
least four more steps. Shrinking rewrites the middle of the prompt, so the
server re-prefills everything after the first changed step. Shrinking in batches
of four from step 8 on, as before, did so every four steps even when the context
was mostly free: a live run of three Qwen 27B players on one slot of a 64k server
(contexts of 12–22k tokens) spent about 9 of 36 slot-minutes re-prefilling after
those shrinks. In a mock 24-step turn whose request reaches half the budget at
step 16 (`local_agent::tests::requests_stay_append_only_until_the_shrink_threshold_then_shrink_once`)
the prefix breaks once instead of five times, and prompt bytes to prefill fall
from 302k to 208k (reuse from the previous request 72.5% → 86.4%), although the
requests are larger. With a zero threshold
(`local_agent::tests::shrinking_in_batches_keeps_the_prefix_stable_between_batches`)
batches of four still beat one step per step (32.4% → 72.5%). Recall is unchanged:
steps leave the recent window (four to seven steps) in batches of four whether or
not they were shrunk, and become summary-tree leaves holding their originals.
The loop only serves local and private endpoints; Codex and Claude manage their
own context, and hosted prefix caches would favour an append-only prefix too.

When a request reaches 75% of the input budget, the oldest block is folded into a
summary on the chat's compactor (never splitting a call from its results). A
block too large for the compactor is built from its children's summaries, and a
folded block stays folded so the prefix stays stable for caching. Each summary
targets about a quarter of the compactor's output limit; on failure it is retried
at a third of that length, then replaced by a labelled deterministic digest of
call names and result heads. If every older block is folded and the request is
still over the threshold, the verbatim window narrows, and as a last resort the
newest results are shrunk. Only if it still does not fit does the attempt fail.
Only the final report enters chat history.

In-turn summaries use the structured format of every tree node
([How it works](#how-it-works)), so a parent block merges its children's
findings. The agent can also mark what must survive: the system prompt asks it
to write each key fact or diagnosis on a line starting `Finding:` and each choice
on one starting `Decision:` (`Diagnosis:`, `Root cause:` and `Plan:` count too,
after any list or emphasis markup). Control collects those lines from the
replies and from the reasoning (which is never transcript) of the steps a node
covers and carries them verbatim under the node, newest first within 1.6 KB,
whatever the summary keeps; a deterministic digest carries them too. In a mock
turn of over 30 steps (`local_agent::tests::a_diagnosis_survives_many_in_turn_compactions`,
`local_agent::context::tests::a_marked_finding_survives_every_fold_even_when_summaries_drop_it`)
a diagnosis from step 1 is still in the request after every fold although the
compactor's summaries drop it.

The turn's steps are nodes of the conversation's summary tree too, in the same
node format and checks: each step that leaves the verbatim window becomes a leaf
holding its original messages (up to 96 KiB), and each folded block a node over
its two retained children, or over one original per step when it was summarized
directly. They are content-addressed files under `summaries/turns/ATTEMPT/`.
Only the roots of each attempt's cut are registered, in memory, for the eight
most recent local attempts; an evicted attempt's files are deleted, and files a
restart left unregistered are deleted after an hour. `control_history_read`
lists, reads and searches them ([Inspect and recall](#inspect-and-recall)). The
DM's Memory Tree (`/dm/ID/memory`) lists their roots under **Local turn step
trees** (from memory, no disk reads), each linked to the evidence reader, which
expands it to the original steps; its metadata carries them as `turn_trees`.
Operator edits (pins, drops, revisions) index conversation entries and do not
apply to step trees. A failed write records a
`local-recall` activity and never fails the turn.

Once per turn, when a request reaches 80% of the budget or the turn first folds a
block, Control appends a note asking the model to publish a checkpoint with its
key findings and current plan in the working notes (`control_turn_handoff`
`payload.findings` and `payload.next`; an omitted payload keeps the older notes),
and to mark findings with `Finding:` lines meanwhile. The step-cap note asks the
same. Codex and Claude manage their own in-session context.

**Compaction.** A chat on a local model is compacted by the same endpoint through
the summary tree by default, without being listed under automatic summaries;
in-turn summaries use it too. Listing the chat under **Automatic summaries** with
another model overrides this. The DM's Memory Tree (`/dm/ID/memory`) reports the policy
(`local chat model default`, `operator opt-in` or `off`).

**Usage and provenance.** Step usage is summed into the attempt's token counts.
A `local_model` activity records the name, alias, declared revision, server
model and build fingerprint, step and compaction counts, elapsed time and tokens,
with the reasoning share of output tokens when the model thought.

**Limits.** Tool calls run sequentially even when the model emits several. Token
estimates are byte counts, not a tokenizer. Images are not sent. Core schemas use
about 3.2–3.4k tokens for members and leads; schemas loaded during a turn stay
loaded until it ends.

### Thinking

Each text-model entry has a **Thinking in agent turns** level (`thinking` in
`model.json`): `off`, `minimal` (128 tokens), `low` (512), `medium` (1536, the
default) or `high` (4096). Each step adapts around it
(`src/local_agent/thinking.rs`):

- one level higher on the first step, after a failed or malformed tool call (a
  refused handoff included) and after an in-turn compaction;
- one level lower on a routine step after a successful action;
- `minimal` after a reply cut at the output limit;
- `minimal` on a resumed turn (host checkpoint or retried run) until its first
  tool call runs;
- Qwen's soft switches: a reply of only `/think` re-sends the same step once at
  `high` (asking again is a malformed step), and `/think` or `/no_think` in a
  reply sets the next step.

`off` never thinks. Requests carry llama.cpp's per-request controls (b11669,
`tools/server/server-common.cpp`): `chat_template_kwargs.enable_thinking`,
`reasoning_budget_tokens` (0 ends thinking at once; the server forces the end tag
when the budget runs out) and `reasoning_format: "deepseek"`. Thinking shares
`max_tokens` with the answer, so the budget is at most half the output reserve and
a tool call always keeps the other half; raise the reserve to give thinking more
room. Summaries and `control_model_generate` never think.

Reasoning never enters the step transcript or the attempt's activity (an inline
`<think>…</think>` block is taken out too), so it costs no context. Its tokens
count as output: as reported (`completion_tokens_details.reasoning_tokens`),
else estimated. A turn cut off by its time limit, step cap or a provider failure
carries its latest step's reasoning (the last 1500 characters, verbatim) into the
host checkpoint, and the next turn is told to continue that plan. Steps are not
streamed, so a step still in flight when the turn stops loses its reasoning; the
previous step's is carried.

### Prefix caching

llama.cpp keeps the previous request's KV cache in its slot and reuses the longest
common token prefix (`cache_prompt`, on by default), so only the changed suffix is
prefilled. Control orders every turn input for this, on all backends. Content that
is stable across a chat's wakes comes first and is byte-identical between wakes:
system message and core schemas, adapter framing, lens core and guidance index,
deferred tool index, session rules, original assignment, then history (which only
grows). Content that changes comes last: working notes, recovery notes, wake or
continuation notes, the current event and its room page, then the fresh
[observation](WATCHES.md#fresh-observations). The test
`turn_context::tests::policy_size::consecutive_wakes_share_their_stable_prefix`
pins this order; [PERFORMANCE.md](PERFORMANCE.md) has the measurements. Within a
local turn, the observation is the request's last message and is not stored, so
each request is a prefix of the next.

The prefix still breaks when the history window slides (once full, the oldest
entry drops at a wake), when a compaction summary changes, when a local turn
reaches half its budget and shrinks its older steps, or when a deferred tool's schema loads
mid-turn (chat templates render tools in the system block). The slot also holds
the newest step's generated thinking, which the next request leaves out, so reuse
ends at that step's assistant message and only the newest step is prefilled again.
Use one slot per concurrently active chat (`-np`) so different chats do not evict
each other's prefix; llama-server routes each request to the best-matching slot.
Hosted backends benefit from the same order: OpenAI caches prompt prefixes of
1024 tokens or more automatically, and Claude Code caches its system prompt and
tool definitions.

## Measurements

Single runs on an Apple M3 Max (64 GB unified memory, live node running), pinned
llama.cpp build 11669 (`620d222`), Qwen3.8-27B Q4_K_M, `--ctx-size 32768 -np 1`.
These are indicative, not controlled benchmarks.

| Measure | Metal (host) | CPU container (8 cores) |
| --- | ---: | ---: |
| Decode | 14–17 tokens/s | 2.1–2.2 tokens/s |
| Prompt processing | 150–200 tokens/s | not measured |
| `control models bench NAME 5` (~100-token prompts) | median 4.8 s | not measured |
| Memory | ~18 GB mapped weights + ~5.4 GB process | 20–23.3 GiB container |

A cold first agent step is dominated by prompt processing of tool schemas; later
steps and turns reuse the slot's prefix cache and take a few seconds each.
`control models bench` measures through Control's adapter but cannot see server
memory; use `footprint PID` on macOS, `docker stats` or the server log. For raw
throughput, run `llama-bench -m MODEL.gguf -p 512 -n 128` from the same package
with the server stopped. Further figures are in [PERFORMANCE.md](PERFORMANCE.md).
