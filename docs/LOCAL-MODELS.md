# Local text models and history compaction

Control calls a configured text endpoint; inference runs in a separate service or
tool container. It does not embed llama.cpp, download weights or grant a
summarizer tools. The same derived history view feeds Codex and Claude chats.
This does not replace either CLI's private context handling. The same summary
worker also accepts explicitly selected Claude/Codex CLI text adapters. General
local tool-using agents and `/model` selection are separate follow-up work.

For a future local agent adapter, Control owns input assembly, tree compaction
and context-fit admission; the inference server supplies generation. Server KV
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
limits are operator choices, not a guarantee a particular model fits. CPU-only
inference is intentional in this image; GPU runtimes/resources remain external.
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
