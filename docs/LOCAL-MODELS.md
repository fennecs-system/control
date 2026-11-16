# Local text models and history compaction

Control calls a configured text endpoint; inference runs in a separate service or
tool container. It does not embed llama.cpp, download weights or grant a
summarizer tools. The same derived history view feeds Codex and Claude chats.
This does not replace either CLI's private context handling. The same summary
worker also accepts explicitly selected Claude/Codex CLI text adapters. Agents and
the operator can also call a configured endpoint directly for bounded text
generation (`control_model_generate`, below). A tool-capable local endpoint can
also drive agent turns through Control's native tool loop; see
[Local agent turns](#local-agent-turns). `nix run .#inference` starts the node's
llama.cpp service with the platform's accelerated backend.

Chat and TUI accept `/model` to list configured agent models, `/model PROVIDER:ID`
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

In **Manage → Models & providers**, add a local text model with:

- a unique name and the model alias served by the endpoint;
- a loopback/private literal address ending in `/v1` (or `localhost`);
- a weights/runtime revision, ideally the GGUF SHA256 plus runtime revision;
- declared context/output token limits, input/output byte ceilings and an
  explicit per-request timeout. There is no retry or cloud fallback.

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

Every **local endpoint** summary model (adapter *Local endpoint*, not the Codex or
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

`nix run .#inference -- --model /absolute/model.gguf` starts the node's llama.cpp
server with the backend this platform needs. The binary always comes from the
pinned nixpkgs, weights are never downloaded, and the server binds loopback only.

| Backend | Selected when | What runs |
| --- | --- | --- |
| `metal` | auto on aarch64-darwin | the pinned darwin `llama-server` (built with `libggml-metal`) as the launchd agent `dev.control.inference`, `-ngl 999`, logs in `~/.local/share/control/inference/server.log` |
| `container` | auto elsewhere | the pinned Linux image `control-inference` (read-only, no capabilities, model directory mounted read-only); CUDA (`--gpus all`) when `nvidia-smi` works and Docker has the NVIDIA runtime, otherwise CPU |
| `cpu` | explicit | the CPU container on any platform |

Override with `--backend metal|container|cpu` (or `CONTROL_INFERENCE_BACKEND`);
`--acceleration cpu|vulkan|cuda` picks the container build explicitly (`vulkan`
passes `/dev/dri` and ships Mesa's ICDs; CUDA is unfree and needs the NVIDIA
container toolkit). The CUDA and Vulkan images evaluate but were not built or run
on this Mac. Other flags: `--alias` (default `qwen-local`), `--ctx-size` (32768),
`--port` (8081), `--parallel` (1 slot), `--sha256` (skip hashing) and extra
`llama-server` flags after `--`. `--dry-run` prints the launchd plist or the
container command and changes nothing; `--status` and `--stop` manage the service.

Every start uses `--jinja` (tool calls through the model's chat template) and
`--reasoning-budget 0`. The script hashes the GGUF once and writes the backend,
model path, SHA256, alias, port and a ready-to-paste revision string to
`inference.json` in the Control configuration directory; the saved backend is reused
when `--backend` is omitted. It prints the values for **Manage → Models &
providers → + New summary model**: adapter *Local endpoint*, the alias as name and
model ID, `http://127.0.0.1:PORT/v1`, the revision, the context size, and
**Serves tool calls** ticked. Control does not edit `model.json` from the script.

The maintainer approved running llama.cpp natively on Apple Silicon because the
container cannot reach the GPU there. It is the only host-native third-party
service, it is pinned by `flake.lock`, and it involves no Python or Node. Unified
memory is shared with Control, Docker's VM and other services: keep weights plus
KV cache well below physical RAM. `-np 1` serializes agent steps and background
summaries on one slot; more slots split `--ctx-size` between them.

## Run a Qwen 27B-class GGUF

1. Choose weights before downloading: Q4_K_M of a 27B model is about 19.4 GB of
   weights (above); Q5_K_M/Q6_K need roughly 23/26 GB (scaled from that size).
   Add KV cache: at 32 768 context the measured run below needed about 5.4 GB
   more. Verify the published SHA256 and keep weights outside the checkout (for
   example `~/.local/share/control/models/`).
2. `nix run .#inference -- --model ~/.local/share/control/models/Qwen3.8-27B-Q4_K_M.gguf`
3. Add the endpoint in Manage with the printed values and **Serves tool calls**.
   Agent steps re-send about 10 k tokens of tool schemas, so keep a generous
   timeout (the default 1800 s is fine on Metal; CPU needs far more).
4. Check it: `control models test qwen-local`, then `control models bench qwen-local 5`.
5. In a chat, `/model local:qwen-local`. Future turns run on the local model;
   `/model default` returns to the node default.

Without **Serves tool calls** the endpoint stays *text generation only*:
`control_model_generate` works, and `local:NAME` is refused for agent turns.

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
attempt (room, continuity, services, spawn, workspace, knowledge, …) plus
`control_workspace_run`, the same credential-free offline shell Claude Code uses.
Names and parameter schemas are unchanged; long descriptions keep their first
sentences. Every call goes through the worker tool dispatch the CLI mailboxes use,
so authorization, attempt fencing, the staged-handoff fence, judge policy, room
idempotency and nudges are identical. Calls run one at a time in the order the
model emits them (at most eight per step). A text reply without tool calls is the
final report; a committed `control_turn_handoff` finishes the attempt immediately.
Workspace changes are then checkpointed and published exactly as for Codex.

**Malformed output.** Arguments that are not a JSON object, unknown tool names,
more than eight calls, an empty reply or a reply cut at the output limit never
execute. The model receives a corrective tool result or note and may retry; four
malformed steps in a row fail the attempt visibly. A valid step resets the count.
A response without exactly one choice is an immediate failure. The loop also stops
after 64 model steps.

**Cancellation and deadlines.** The loop checks cancellation, design questions,
blocking delegation and the attempt deadline before every step and after every
tool call. A cancelled run closes the in-flight HTTP connection, so llama.cpp
stops generating. Attempt-owned tool containers are stopped when the loop ends;
persistent services are separate resources. Recovery starts a fresh attempt from
Control's journal; server KV caches only speed up repeated prefixes.

**Context fit.** The endpoint's declared limits are the budget. Turn-input
planning reserves `OVERHEAD_TOKENS` (14 336) for tool schemas and the first
exchange, so history is selected newest-first within what remains and omissions
are recorded. Before each request the loop estimates tokens conservatively
(3 bytes per token) for messages plus schemas. Input that cannot fit at admission
blocks the run before any model call. When a long turn outgrows the context, whole
earlier steps of that turn (never the request, and never a call separated from its
results) are summarized by the chat's compactor and replaced with one labelled,
untrusted summary. If nothing more can be compacted, the attempt fails visibly.
The originals stay in the attempt's activity.

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
inputs are not sent. A 32k context holds roughly 10 k tokens of tool schemas, so
long single turns rely on in-turn compaction.

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
and later turns cheap while the slot keeps that prefix.

The first run exposed a real defect: room posts require a numeric nonce, and the
loop's nonces were rejected. The model saw the errors, retried, then handed off
and reported the failure honestly; the fix and the second run are above.
