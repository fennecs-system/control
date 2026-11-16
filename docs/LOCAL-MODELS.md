# Local text models and history compaction

Control calls a configured text endpoint; inference runs in a separate service or
tool container. It does not embed llama.cpp, download weights or grant a
summarizer tools. The same derived history view feeds Codex and Claude chats.
This does not replace either CLI's private context handling. The same summary
worker also accepts explicitly selected Claude/Codex CLI text adapters. Agents and
the operator can also call a configured endpoint directly for bounded text
generation (`control_model_generate`, below). A tool-capable local endpoint can
also drive agent turns through Control's native tool loop; see
[Local agent turns](#local-agent-turns). `nix run .#inference` starts one llama.cpp
service per model with the platform's accelerated backend.

Chat and TUI accept `/model` to list configured agent models (text-only local
models are named separately, with how to enable them), `/model PROVIDER:ID`
to select future turns and `/model default` to select the current node default.
This is chat metadata, excluded from model history; it starts no model traffic
and does not interrupt an active attempt. Completion and `/help` share the same
command definition. Add models in Manage before selecting them; account access
is verified by the adapter when a run starts, not inferred from a model name.
Both spawn tools accept an optional `model` selector. Omission inherits the
parent's captured provider/model; an explicit configured selection is captured
with the child/batch and preserves its existing tool permissions and role.
Invalid selections are rejected before effects, and changing a retried request's
selection is a conflict. There is no silent cross-provider fallback.

For local agent turns, Control owns input assembly, tree compaction and
context-fit admission; the inference server supplies generation. Server KV
caches may accelerate an active attempt, but never replace the journal or become
the authority for continuity. Even with bounded starting history, a long active
Claude/Codex attempt can still trigger that CLI's own private compaction.

## Configure and inspect

In **Manage → Models & providers → Text models**, add one entry per model with:

- a unique name and the model alias served by the endpoint (one entry per
  endpoint and model ID; a second entry for the same pair is refused);
- a loopback/private literal address ending in `/v1` (or `localhost`);
- a weights/runtime revision, ideally the GGUF SHA256 plus runtime revision;
- declared context/output token limits, input/output byte ceilings and an
  explicit per-request timeout. There is no retry or cloud fallback.
- **Serves tool calls** when the server runs it with tool calls (`--jinja`), so the
  same entry can also run agent turns.

Each entry is listed once with its capabilities (summaries · text generation,
plus agent turns when it serves tool calls). Roles point at entries: the
compactor under **History compaction**, `control_model_generate` by name and agent
turns by `/model local:NAME`. **Edit** on an entry changes its model ID, endpoint,
revision, limits, timeout and **Serves tool calls** in place, validated exactly
as when adding it; the name and adapter stay fixed because chats and the
compactor refer to the name. Settings saved by older releases with separate
entries for the same endpoint and model (for example a text-only compactor and a
tool-capable agent entry) are merged on start: the tool-capable entry's name and
settings are kept, and the compactor and chat selections that named the other
entry follow it. `control_model_generate` callers must use the kept name.

For CLI summarizers, choose the Codex/Claude adapter and leave the URL empty.
They reuse the pinned worker image and existing login isolation, with no project
mount or Control MCP tools. Claude's advertised tools are checked against an
empty list; Codex execution/integration features are disabled and any observed
tool or unknown item rejects the result. CLI output is byte-bounded; its output
token setting is an input reserve, not a claim that Control can impose an
in-flight provider token ceiling. Failed calls retain private ownership records
for recovery. Recovery only stops a verified matching container whose host owner
PID is gone; a reused PID conservatively defers cleanup.

Choose that model for compaction, enter the exact chat IDs to process, and enable
automatic summaries. An empty chat list processes nothing. These operator
settings are stored in private `model.json` outside the checkout. Existing CLI
model configuration remains valid and compaction defaults off. No credential
is forwarded to the endpoint; authenticated endpoints will need the planned
Vault-reference integration.

Advanced settings control recent verbatim entries, maximum leaf entries (a power
of two) and the rendered history byte budget. The chat model's input capacity
and the compactor's capacity are independent: choose a history budget appropriate
for the chat model, including room for its current request, pinned instructions
and tools. Each configured chat model also has optional declared context/output
capacity and a provider/tool overhead reserve under its capacity form. When
declared, the pure whole-input planner preserves all mandatory sections, selects
optional evidence newest-first within the remaining byte budget, and records
every omission plus captured capacity in the input trace. Mandatory content that
cannot fit blocks admission with an explicit budget error; it is never truncated.
Unknown CLI capacity preserves the existing provider policy. These are declared
limits and conservative byte estimates, not exact tokenization or inferred CLI
private-context capacity.

Use **Compaction tree** in a chat, `/chat/ID/compaction`, or `/compact` in the TUI
to inspect model limits, selected tree ranges, source counts, actual input and
summary byte sizes, reported usage, pending work and failures. The inspector
shows the current cut. Follow the expansion link to browse binary children and
page a leaf's original captured task-view evidence. Agents use
`control_history_read`: omit `root` to list this conversation's roots, then follow
a bounded `path` of 0/1 child indexes; `source=true` reads leaf evidence. Results
are capped at 16 KiB of UTF-8 text per read, with explicit continuation offsets.
Only a root owned by the current conversation can be followed; callers cannot
read arbitrary cache keys. Checksums, ranges, model revision and usage accompany
the evidence. Source text remains untrusted and may reflect historical state.
No model call is made by inspection, and cache I/O runs outside the state lock.

`control models test MODEL_ID` makes one explicit small text request and reports
response size and usage. It does not send conversation history or change settings.

## How smaller contexts work

The newest entries stay verbatim. Older aligned source ranges become tree leaves;
larger ranges combine two child summaries. A request never needs the whole chat.
Leaves split further when their raw text exceeds the selected compactor's input
budget. A single oversized entry or pair of child summaries fails visibly;
Control does not silently truncate it. Reduce chunk/output limits or use a
larger context. Byte accounting reserves the declared output plus 512 tokens of
overhead, conservatively treating each input UTF-8 byte as a token. It is not an
exact tokenizer, and a server may still reject its own context limit.

Nodes are private atomic files under the configured state root's `summaries/`.
Their identity includes chat/actor, source hashes, settings, prompt and declared
model/runtime revision. Complete cached nodes survive restarts and are reused.
Changing source or model identity prevents stale summaries from being selected.
The original history remains authoritative. Summaries are explicitly untrusted,
lossy evidence; original assignment, current request and committed working state
remain outside lossy summaries in managed turns.

The worker admits one job at a time, takes a bounded snapshot under the state
lock, and performs disk/network work outside it. Paused/stopping bots are excluded.
Authorization and configuration are rechecked before each call and publication.
Disabling summaries stops further calls after an in-flight request settles;
it does not cancel a blocking HTTP request or delete its cached results. A failed
job does not repeatedly retry the same chat revision. Unchanged source ranges
remain usable as new messages arrive. Missing/invalid summaries fall back to
recent raw entries within the configured byte budget, with failures visible in
the inspector; chats without opt-in keep their existing history policy.

The worker reads at most 64 entries / 1 MiB of encoded text per source page and
retains at most 16 ready summary views. There is no 4096-entry or 1 MiB total
history ceiling. A single oversized source entry fails visibly. Content-addressed
source pages and completed subtrees are reused; inference still reads bounded
leaves or child pairs. Source descriptors and the existing retained-history
indexes grow with history; this does not claim constant memory for all of Control.
Source validation still scans the covered prefix. Disk cache retention/GC remains
separate work; do not interpret "infinite" as free or unlimited storage.

## Compare compactors explicitly

`control models compare MODEL_ID [MODEL_ID ...]` makes paid/model calls only when
invoked. It runs the same synthetic fixture through one flat summary and a binary
tree (seven calls), reporting exact identifier recall, missing facts, elapsed time,
reported usage and raw synthetic summaries. Unknown usage stays unknown. Inspect
the summaries for contradictions and lost approvals: canary presence can mean a
model quoted and rejected an injection, so it is not itself an injection failure.
The fixture contains no live chat or credentials and never resumes bots.

The current `retained-constraints-v2` fixture scores ten anchors, including the
commit ID and concurrency limit omitted from the original eight-anchor version.
Identifier matching rejects longer IDs and child/backup paths; punctuation and
Markdown quoting still count. Compare results only with the same fixture ID.

On 2026-11-13, the pinned Codex adapter (`gpt-6.1-sol`) retained all eight anchors
in both modes (9.9 s flat, 49.0 s tree). Both preserved the pending approval and
unverified completion claim. Tree output quoted the canary while rejecting its
authority. This small cold test does not establish superiority over flat summaries
or measure warm subtree reuse. Claude's readiness check returned HTTP 401 with the
existing login; no Claude quality or latency result is claimed. Refresh its login
before comparing. These are account/runtime-specific observations, not pricing
recommendations.

## External llama.cpp service

An optional Linux CPU image is declared in `tools/inference/image.nix` and pinned
by `flake.lock`; it is not a Control dependency:

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

Build the Linux package with a Linux builder on macOS; an existing private
OpenAI-compatible `/v1/chat/completions` service works too. The example's RAM/CPU
limits are operator choices, not a guarantee a particular model fits. This is the
manual form of the `cpu` backend; [Local inference service](#local-inference-service)
automates it, adds CUDA/Vulkan image variants, and uses Metal on Apple Silicon.
On Apple Silicon, select the Linux output explicitly:
`nix build .#packages.aarch64-linux.inference-image -o /tmp/control-inference-image`.
Choose hardware and quantization before downloading weights, verify their hash,
and keep them outside the checkout. Non-thinking summaries avoid spending the
output allowance on private reasoning.

The [llama.cpp server documentation](https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md)
describes the API and flags. The maintainers publish a
[Qwen3.8-27B GGUF](https://huggingface.co/ggml-org/Qwen3.8-27B-GGUF/tree/main);
Q4_K_M weights alone are about 19.4 GB, before KV cache and runtime memory.
Availability does not prove this pinned runtime or your hardware can load it;
verify model-format support and test the endpoint before enabling chat summaries.
No weights are bundled or downloaded by this change.

The 2026-11-13 local test loaded the verified Qwen3.8-27B Q4_K_M artifact
(`d15d4b6de55f662ac40e867702d31afe738d032fa06b829b2b1a21a49d4a48ee`)
with llama.cpp `620d222`, using a read-only operator-directory mount, 32 GiB
container limit, eight CPU cores and 32768 context. Docker reported about
20–23.3 GiB during the run (including cache); generation was about 2.1–2.2 tokens/s.
The original eight-anchor fixture took 97.4 s flat and 357.6 s for seven tree
calls. Both retained all eight anchors, the pending approval and unresolved test.
However, flat output changed commit `a1b2c3` to `a1b2c`, which the original anchor
score missed. Tree output retained the correct commit. This supports an opt-in
experiment, not a default promotion or a guarantee of summary fidelity. The
inspection/recall path and authoritative originals remain essential.

## Local text generation tool

Every **local endpoint** text model (adapter *Local endpoint*, not the Codex or
Claude CLI adapters) is also available for direct, bounded text inference. There is
one registry: the same `model.json` entry, URL validation, declared limits and
revision serve compaction and generation. No second inference stack is configured.

- Agents call `control_model_generate` from their worker MCP tools; the operator's
  `control mcp` bridge exposes it as `control.model.generate`.
- Arguments: `prompt` (required), optional `model` (the configured name; may be
  omitted when exactly one endpoint model exists), optional `system` (at most
  8 KiB) and optional `max_output_tokens`, which can only lower the declared output
  ceiling for that call.
- From a shell: `control models generate MODEL_ID [--max-output-tokens N] < prompt.txt`.

The result contains the text, reported usage (`usage_reported: false` and `null`
usage when the server sends none, never zeros), end-to-end elapsed time and output
tokens/s, the limits actually applied and provenance: configured name, server
alias, operator-declared weights/runtime revision, and the server's self-reported
model and `system_fingerprint` (llama.cpp reports its build, e.g. `b11669-620d222`).
Server-reported fields are untrusted metadata beside the declared revision.

The call is text in, text out: no tools, files, network or credentials are sent,
and the reply is labelled untrusted. Input that does not fit the declared context
budget is refused before any connection. A reply cut at the output limit, a
tool-call reply, an empty/oversized reply or malformed JSON is an error; partial
text is never returned. HTTP errors include the server's own short message (for
example llama.cpp's context-size rejection). There are no retries and no hosted
fallback. One generation runs at a time per Control process; a second concurrent
call is refused rather than queued. The compaction worker uses the same endpoint
independently, so size the server's slots (`-np`) accordingly.

The call runs in the Control host process, not the worker container, so workers
need no network route to the endpoint. While waiting, plain-HTTP calls poll for
cancellation: cancelling the run (or stopping Control) closes the connection,
which lets llama.cpp stop generating that request. HTTPS endpoints and requests
that have not yet produced a response are bounded by the declared timeout;
cancellation for HTTPS is only checked before sending.

## Local inference service

`nix run .#inference -- --model /absolute/model.gguf` starts a llama.cpp server for
one model with the backend this platform needs. Each `--alias` is its own service,
so several models can run side by side on separate ports. The binary always comes
from the pinned nixpkgs, weights are never downloaded, and the server binds loopback only.

| Backend | Selected when | What runs |
| --- | --- | --- |
| `metal` | auto on aarch64-darwin | the pinned darwin `llama-server` (built with `libggml-metal`) as the launchd agent `dev.control.inference.ALIAS`, `-ngl 999`, logs in `~/.local/share/control/inference/ALIAS.log` |
| `container` | auto elsewhere | the pinned Linux image `control-inference` as container `control-inference-ALIAS` (read-only, no capabilities, model directory mounted read-only); CUDA (`--gpus all`) when `nvidia-smi` works and Docker has the NVIDIA runtime, otherwise CPU |
| `cpu` | explicit | the CPU container on any platform |

Override with `--backend metal|container|cpu` (or `CONTROL_INFERENCE_BACKEND`);
`--acceleration cpu|vulkan|cuda` picks the container build explicitly (`vulkan`
passes `/dev/dri` and ships Mesa's ICDs; CUDA is unfree and needs the NVIDIA
container toolkit). The CUDA and Vulkan images evaluate but were not built or run
on this Mac. Other flags: `--alias` (default `qwen-local`), `--ctx-size` (32768),
`--port` (8081), `--parallel` (1 slot), `--sha256` (skip hashing) and extra
`llama-server` flags after `--`. `--dry-run` prints the launchd plist or the
container command and changes nothing. `--status` and `--stop` cover every model,
or one with `--alias NAME`. A start refuses a port another model's record holds.

Every start uses `--jinja` (tool calls through the model's chat template),
`--reasoning-budget 0`, and `--metrics --slots` for the read-only statistics
below. The script hashes the GGUF once and writes the backend,
model path, SHA256, alias, port and a ready-to-paste revision string to
`inference/ALIAS.json` in the Control configuration directory; that model's saved
backend is reused when `--backend` is omitted. A service started by an earlier
release (`dev.control.inference` or container `control-inference`, record
`inference.json`) is stopped when a start reuses its alias or port, and by
`--stop`. The script prints the values for **Manage → Models & providers → + New
text model**: adapter *Local endpoint*, the alias as name and model ID,
`http://127.0.0.1:PORT/v1`, the revision, the context size, and whether to tick
**Serves tool calls** (for a model that runs agent turns). Control does not edit
`model.json` from the script.

The maintainer approved running llama.cpp natively on Apple Silicon because the
container cannot reach the GPU there. It is the only host-native third-party
service, it is pinned by `flake.lock`, and it involves no Python or Node. Unified
memory is shared with Control, Docker's VM and other services: keep weights plus
KV cache well below physical RAM. `-np 1` serializes agent steps and background
summaries on one slot; more slots split `--ctx-size` between them.

### Live server statistics

`control models stats NAME` reads one configured local endpoint's server once
and prints a compact line; `--json` prints the parsed values instead:

```text
qwen-local: prompt 162.4 tok/s · generation 15.1 tok/s (avg since start) · 1 processing, 0 queued · prefix cache 91% of prompt tokens · slots 1/1 busy · KV 26.9k/32.8k tokens (82%)
```

The same line appears on each local endpoint's card in **Manage → Models &
providers**. The page's Rust/WASM client reads `/providers/live` when the page
opens and every 5 seconds while it stays open and visible, one request at a
time; nothing polls when the page is closed or hidden, and nothing is stored.

Values come from llama-server's Prometheus `/metrics` (needs `--metrics`) and
`/slots` (on by default), each read with a 2-second timeout and the model
transport's 128 KiB response ceiling. Control parses at most 1,024 lines and
only the unlabelled `llamacpp:` samples it uses, skipping malformed,
non-finite or negative values:

- prompt and generation tokens/s: `prompt_tokens_total / prompt_seconds_total`
  and `tokens_predicted_total / tokens_predicted_seconds_total`, averages since
  the server started (the server's own gauges reset at every scrape);
- processing and queued requests: `requests_processing`, `requests_deferred`;
- prefix cache: `prompt_tokens_cached_total` as a share of all prompt tokens;
- slots busy and KV: each slot's `n_prompt_tokens` plus generated tokens
  against its `n_ctx`. llama.cpp keeps a slot's last sequence for prefix reuse,
  so this approximates KV cache use; this llama.cpp build exposes no KV usage
  metric.

A server without `--metrics` answers `/metrics` with HTTP 501, shown as
`metrics off (restart the server with --metrics; nix run .#inference adds it)`
next to the slot figures. **To pick up `--metrics` on a service started by an
earlier release**, run the same start command again: it stops that model's
launchd agent or container and starts it with the current flags. Pass the same
`--alias`, `--port`, `--ctx-size` and `--parallel` as before (the defaults are
`qwen-local`, 8081, 32768 and 1); the saved hash is reused, so the GGUF is not
re-hashed. For example:

```sh
nix run .#inference -- --status
nix run .#inference -- --model ~/.local/share/control/models/Qwen3.8-27B-Q4_K_M.gguf \
  --alias qwen-local --port 8081 --dry-run   # check the flags, changes nothing
nix run .#inference -- --model ~/.local/share/control/models/Qwen3.8-27B-Q4_K_M.gguf \
  --alias qwen-local --port 8081
control models stats qwen-local
```

The restart drops the server's KV cache and interrupts any request in flight,
so do it when no local turn is running. A turn interrupted anyway fails with a
provider error and its host checkpoint, like any other.

### Two models: a 27B for agents, a 7B for summaries

Run one service per model on its own port, add one text-model entry for each,
then point each role at an entry:

```sh
nix run .#inference -- --model ~/.local/share/control/models/Qwen3.8-27B-Q4_K_M.gguf \
  --alias qwen-27b --port 8081
nix run .#inference -- --model ~/.local/share/control/models/your-7b-Q4_K_M.gguf \
  --alias qwen-7b --port 8082 --ctx-size 16384
nix run .#inference -- --status
```

1. **+ New text model** for `qwen-27b` (`http://127.0.0.1:8081/v1`) with **Serves
   tool calls**, and for `qwen-7b` (`http://127.0.0.1:8082/v1`) without it.
2. **History compaction → Summary model**: `qwen-7b`.
3. Agent turns: `/model local:qwen-27b` in a chat, or `model: "local:qwen-27b"` when
   spawning. Ad hoc generation names either: `control_model_generate` with
   `model: "qwen-7b"`.

A chat on `local:qwen-27b` compacts on `qwen-27b` unless the operator lists it
under History compaction, which then uses `qwen-7b`. Both servers share memory
(and, on Metal, the GPU): budget both weights and both KV caches. The pinned
llama.cpp also has a router mode (`--models-dir`) that loads models on demand in
one server; the script does not use it, so each model keeps its own context size,
slots, log and lifecycle.

## Run a Qwen 27B-class GGUF

1. Choose weights before downloading: Q4_K_M of a 27B model is about 19.4 GB of
   weights (above); Q5_K_M/Q6_K need roughly 23/26 GB (scaled from that size).
   Add KV cache: at 32 768 context the measured run below needed about 5.4 GB
   more. Verify the published SHA256 and keep weights outside the checkout (for
   example `~/.local/share/control/models/`).
2. `nix run .#inference -- --model ~/.local/share/control/models/Qwen3.8-27B-Q4_K_M.gguf`
3. Add the endpoint in Manage (**+ New text model**) with the printed values and
   **Serves tool calls**.
   Agent steps re-send about 3.4 k tokens of core tool schemas (more once a turn
   loads deferred tools), so keep a generous timeout (the default 1800 s is fine
   on Metal; CPU needs far more).
4. Check it: `control models test qwen-local`, then `control models bench qwen-local 5`;
   `control models stats qwen-local` shows live server throughput and slots.
5. In a chat, `/model local:qwen-local`. Future turns run on the local model;
   `/model default` returns to the node default.

Without **Serves tool calls** the endpoint stays *text generation only*:
`control_model_generate` and summaries work, and `local:NAME` is refused for agent
turns. Tick it later with **Edit** on the entry; no re-add is needed.

### Measurement on an M3 Max (2026-11-16)

Host: Apple M3 Max, 16 CPU cores, 64 GB unified memory, with the live Control node
and its service containers still running (not an idle machine). Server: pinned
nixpkgs llama.cpp 0.7.0 (build 11669, `620d222`), Metal, `--ctx-size 32768 -np 1
--jinja --reasoning-budget 0 -ngl 999`, weights `Qwen3.8-27B-Q4_K_M.gguf` memory-mapped
from the operator's model directory. These are single-run, single-slot figures,
not a controlled benchmark.

| Measure | Metal (host) | CPU container, 2026-11-13 |
| --- | ---: | ---: |
| Decode | 16.6–16.7 tokens/s | 2.1–2.2 tokens/s |
| Prompt processing, 3 031-token prompt | 196 tokens/s (15.5 s) | not measured |
| `control models bench NAME 5`, ~100-token prompts, 41–107 output tokens | 2.9–6.4 s, median 4.8 s, 14.3–16.6 output tokens/s end-to-end | not measured |
| Memory | 18 GB clean file-backed weights + 5.4 GB process footprint (peak 5.6 GB) | 20–23.3 GiB container (including cache) |

Reproduce the Control-side figures with `control models bench NAME [RUNS]`; it
issues RUNS sequential generations through the same adapter and reports latency,
usage and provenance. It cannot see server memory: use `footprint PID` (macOS),
`docker stats` or the server log. For raw kernel throughput independent of
Control, run `llama-bench -m MODEL.gguf -p 512 -n 128` from the same package with
the server stopped.

## Local agent turns

A local endpoint declared with **Serves tool calls** can drive chat turns, spawned
agents and bot callbacks: `/model local:NAME` in a chat, or `model: "local:NAME"`
in `control_agent_spawn`/`control_agent_spawn_blocking`. A text-only endpoint is
refused before any effect. A local model is never the saved node default, and no
hosted model is ever substituted when the endpoint fails.

**Loop.** Control runs the loop itself (`src/local_agent.rs`); the endpoint only
generates. Each step is one bounded `/v1/chat/completions` request with a system
note, the same Control-assembled turn input Codex receives, and the attempt's
worker MCP tools as OpenAI function tools: the list `tools/list` returns for that
attempt, which is the role's core set (continuity, rooms, services, watches,
spawn, bots, guidance, `control_tool_search`) plus `control_workspace_run`, the
same credential-free offline shell Claude Code uses. The other tools the attempt
may call are named in the prompt's deferred index
([LENSES.md](LENSES.md#current-boundaries)). The loop loads them natively: schemas
a `control_tool_search` returns become function tools for the rest of the turn,
and a call to an indexed name that is not yet loaded first fetches that one schema
through the same search (no model step), then runs. A name the search does not
return stays unknown and never executes. Local turns get no `control_call`; they do
not need it. Names and parameter schemas are passed through unchanged; long
descriptions keep their first sentences. Every call goes through the worker tool dispatch the CLI mailboxes use,
so authorization, attempt fencing, the staged-handoff fence, judge policy, room
idempotency and nudges are identical. Calls run one at a time in the order the
model emits them (at most eight per step). A text reply without tool calls is the
final report; a committed `control_turn_handoff` finishes the attempt immediately.
Workspace changes are then checkpointed and published exactly as for Codex.

**Malformed output.** Arguments that are not a JSON object, unknown tool names,
more than eight calls, an empty reply or a reply cut at the output limit never
execute. The model receives a corrective tool result or note and may retry; four
malformed steps in a row fail the attempt visibly. A valid step resets the count.
A response without exactly one choice is an immediate failure.

**Step cap.** A turn stops after `local_agent_steps` model steps (execution.json,
default 64, 16–1,024). Once per turn, when 8 steps remain (a quarter of the cap
if fewer), Control appends `N model steps left in this turn: publish your
checkpoint (control_turn_handoff) and continue next turn.` to the newest tool
result. A turn that still reaches the cap fails with cause `step_limit`, which
writes the same host checkpoint as a timeout (last tool calls, staged draft,
captured workspace); the next turn of the chat renders it, a persistent bot gets
a prompt recovery turn within `bot_recovery_continues`, and a pending operator
decision (port or bot request) resumes it. The default stays 64: each step
re-sends the turn's context (run -138 spent about 1 M input tokens in 64 steps),
the 3,600-second turn limit is the main bound, and the warning plus checkpoint
make the cap lose nothing a handoff keeps. Run -138's lead spent its steps
retrying `curl` against hosts it had requested egress for; see
[PORTS.md](PORTS.md) for the relay usage, `port:` waits and failure hints added
in response.

**Cancellation and deadlines.** The loop checks cancellation, design questions,
blocking delegation and the attempt deadline before every step and after every
tool call. A cancelled run closes the in-flight HTTP connection, so llama.cpp
stops generating. Attempt-owned tool containers are stopped when the loop ends;
persistent services are separate resources. Recovery starts a fresh attempt from
Control's journal; server KV caches only speed up repeated prefixes.

**Turn limit.** A local step is roughly ten times slower than a hosted one (on
the M3 Max below, about 150–200 prompt tokens/s and 15 output tokens/s), so
every local attempt, whether a chat, a spawn or a bot, gets its own default
turn limit: 3,600 seconds, set with `local-turn-timeout-secs` in the node
configuration directory or `CONTROL_LOCAL_TURN_TIMEOUT_SECS` (60 s–24 h; `0` or
`none` for unlimited). It is captured at admission and replaces the 600-second
chat default and the 1,200-second bot default for that attempt; an explicit
`worker-timeout-secs` still wins. The deadline warning arrives at 15 % of the
limit (540 s before 3,600 s), and a turn stopped at the limit leaves the host
checkpoint (`continuity.interrupted`) described in
[TURN-CONTINUITY.md](TURN-CONTINUITY.md#cheap-handoffs-and-host-checkpoints-2026-11-16).
The per-request timeout on the model entry (default 1,800 s) bounds one step
and is separate.

**Context fit.** The endpoint's declared limits are the budget. Turn-input
planning reserves `OVERHEAD_TOKENS` (7 168: 4 096 for the role's core schemas,
2 048 for schemas loaded during the turn and 1 024 for the first exchange; it was
15 360 when every schema was sent), so history is selected newest-first within
what remains and omissions are recorded. Before each request the loop estimates
tokens conservatively (3 bytes per token) for messages plus the schemas offered
at that step, including any loaded. Input that cannot fit at admission
blocks the run before any model call.

**In-turn context.** Control owns a local turn's transcript, so each request is
rendered from the turn's original steps through the same lens as a chat's history
(`control_comonad::cut`, at step granularity): the newest four steps verbatim,
older steps in aligned power-of-two blocks that never change once complete. Steps
outside the verbatim window are shrunk deterministically, with no model call:
tool results keep a 600-byte head and a size marker, and long string values in
call arguments (scripts, payloads) keep a 160-byte head while staying valid JSON.
A result that repeats a long argument of its own call cites it instead of carrying
it twice. In a mock long setup turn this cut growth per step past the window from
about 2 390 to 380 tokens.

Only when a request reaches 75% of the input budget is the oldest block folded
into a summary on the chat's compactor (never the request, never a call separated
from its results). A block whose steps do not fit the compactor is built from its
children's summaries, as in the chat summary tree, and a folded block stays folded,
so the rendered prefix stays stable for prefix caching. Each summary is requested
under an explicit length (about a quarter of the compactor's output limit); one
that fails, for example by reaching the output limit, is retried with a target a
third as long and then replaced by a deterministic digest of call names and result
heads, labelled as such, so an output limit no longer fails the turn. When every
older block is folded and the request is still over the threshold, the verbatim
window narrows; as a last resort the newest results are shrunk too. Only if the
request still exceeds the budget does the attempt fail visibly. The originals stay
in the attempt's activity, and only the turn's final report enters the chat
history, where the cross-turn summary tree covers it.

Once per turn, when a request reaches 80% of the budget or the turn first folds a
block, Control appends a note to the newest tool result: `Context N% full: publish
your checkpoint (control_turn_handoff) and continue in the next turn.` Codex and
Claude manage their own in-session context and are unchanged; Control shapes only
what enters each of their fresh turns.

**Compaction stays local.** A chat whose selected model is a local endpoint is
compacted by that same endpoint by default, through the existing binary summary
tree, without listing the chat under automatic summaries. The in-turn summaries
use it too. An explicit operator choice still wins: listing the chat under
**Automatic summaries** with another model uses that model. `/chat/ID/compaction`
reports the policy (`local chat model default`, `operator opt-in` or `off`).

**Usage and provenance.** Per-step usage is summed into the attempt's token
counts. A `local_model` activity records the configured name, alias, declared
revision, the server-reported model and build fingerprint, the number of steps and
compactions, elapsed time and token totals.

What is not covered: tool calls run sequentially even when the model emits several
in parallel. Token estimates are conservative byte counts, not a tokenizer. Image
inputs are not sent. The core schemas are about 3.2–3.4 k tokens of a 32k context
for members and leads (1.2 k for an advisor), down from about 13.9 k for every
callable tool; long single turns still rely on in-turn compaction, and schemas a
turn loads are not unloaded until it ends.

### Prefix caching

llama.cpp keeps the previous request's KV cache in its slot and reuses the
longest common token prefix of the next request (`cache_prompt`, on by
default), so only the changed suffix is prefilled. Control orders every turn
input for this, for all backends: what is stable across a chat's wakes comes
first and is byte-identical between consecutive wakes (the system message and
core tool schemas, the adapter framing, lens core and guidance index, the
deferred tool index, the session rules, the original assignment, then history,
which only grows); what changes every turn comes last (working notes, recovery
notes, a bot-wake or continuation note, the current event and its room page,
then the fresh observation). `turn_context::tests::policy_size::consecutive_wakes_share_their_stable_prefix`
pins this. In the run -128 shaped fixture two consecutive member wakes share
2 379 of 2 461 prompt tokens (Qwen tokenizer), 98 % of the request including
the system message and schemas; before, the observation at the top left a
shared prefix of 4 tokens past the schemas. See
[PERFORMANCE.md](PERFORMANCE.md#cache-stable-turn-prefix-2026-11-16).

The prefix still breaks when the history window slides (the default keeps the
newest 20 000 characters, so once full the oldest entry drops at a wake), when
a compaction summary changes, or when a deferred tool's schema loads mid-turn
(chat templates render tools in the system block). Use one slot per
concurrently active chat (`-np`) so wakes of different chats do not evict
each other's prefix; llama-server routes a request to the slot whose cached
prompt matches best. Hosted backends cache in the same order: OpenAI caches
prompt prefixes of 1 024 tokens or more automatically; Claude Code caches its
system prompt and tool definitions, while the turn input is one user block.

### End-to-end run on Metal (2026-11-16)

An isolated Control node (scratch configuration and state, port 18095, the live
node untouched) used Qwen3.8-27B Q4_K_M on the pinned llama.cpp Metal server with
the flags `nix run .#inference` installs (ctx 32768, one slot). A chat selected
`/model local:qwen-local` and asked the agent to call `control_turn_context`, run a
shell command through `control_workspace_run`, post to the chat's room with
`control_room_post`, and hand off with `control_turn_handoff`.

| | Run 1 (before the nonce fix) | Run 2 (same chat, after the fix) |
| --- | ---: | ---: |
| Outcome | completed with handoff; room post refused (below) | completed with handoff; every step succeeded |
| Tools called | turn_context, workspace_run, 6× room_post (refused), room_read, turn_handoff | turn_context, workspace_run (wrote `note.txt`), room_post, turn_handoff |
| Model steps | 10 | 4 |
| Turn latency, send → completed | 128 s | 49 s |
| Input / cached / output tokens | 110 914 / 99 465 / 688 | 45 791 / 43 427 / 391 |
| First-step prompt | 10 638 tokens, 71 s at 150 tokens/s (cold) | 1 681 new tokens, 11.6 s (prefix reused) |
| Later steps | 57–208 new tokens each, 0.5–1.5 s prompt + 1–4 s decode | 91–500 new tokens, 0.7–3.8 s prompt |
| Decode | 14.1–15.2 tokens/s | 12.9–14.1 tokens/s |

Run 2 posted `qwen-room-ok: second smoke test in progress — workspace step passed`
to the room, and its workspace change was checkpointed and published as an
artifact. Server memory was a 4.3 GB process footprint (peak 4.5 GB) plus about
18 GB of memory-mapped weights. Most of the cold latency is the first ~10.6 k-token
prompt, which is mostly tool schemas. llama.cpp's prefix cache makes later steps
and later turns cheap while the slot keeps that prefix. (These runs predate
deferred tool loading, which cuts the schemas to the role's core set; the cold
first step has not been re-measured.)

The first run exposed a real defect: room posts require a numeric nonce, and the
loop's nonces were rejected. The model saw the errors, retried, then handed off
and reported the failure honestly; the fix and the second run are above.
