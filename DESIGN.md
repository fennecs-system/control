# Control — design conversation

The following records the user's prompts in order, lightly edited for readability. These are product intentions, not claims about implemented functionality.

## Vision

I want to redefine what is possible with agent workflows. We are building Control (the name is stolen from Star Trek: Discovery): the end game of development, MCP, knowledge bases, and agent harnesses and workflows.

I think we need to borrow the Bluesky/federated chain design of servers and MCP clients. It needs to function like the Borg collective: a federated collection of MCP servers with knowledge bases (like Obsidian), agent chat flows, browser-based editors, and categories.

The main hub should expose lenses into different categories. Each category can be a knowledge base, with a lens library to glue contexts together. First, let's focus on building UI elements and discussing the ideas critically.

It needs to plug into different backends—Anthropic, self-hosted, OpenAI, and others—with built-in monitoring. I want to host it behind Tailscale, spawn agents remotely through the UX, and have built-in chat and a text editor. The theme should use monospaced fonts and feel cyberpunk-esque.

## Languages

No garbage languages like Python or JavaScript directly. Rust, WASM, Haskell, Nix, and Lean only.

## Lenses and tools

The lens model can also work for tools, deciding what tools are available in which contexts.

## Declarative agents

I think agents need to run inside a Nix container or a Nix microVM. We also need a notion of personas: a personality description, a policy, and a metaprogramming language for directives.

It would be good if those directives could be verified and enforced. This should cover a spectrum from an open-ended agent with few constraints to more constrained behavior—for example, operate in a while loop and do X. Fundamentally, everything needs to be declarative.

## Environment changes and authority

Agents could request changes to their environment. Perhaps there is a local chain of command or judgment: if they need more tools, they can use Nix inside their sandbox; if they need external resources, it could prompt a user or a judge.

## Collaboration and durable knowledge

Agents also need to collaborate, with a Slack/Discord-style room for sharing information colloquially, and a knowledge base for cemented facts.

We need to keep the knowledge base from filling up with junk text, since the default output of LLMs is a lot of text.

## Build direction and lens semantics

Yes, build it. Take all our prompts and add them to `DESIGN.md`, with a little cleanup to make the language more readable and coherent, while keeping it close to the original.

To answer the earlier question: a lens is a function. It is pure; why would it be owned?

## Working interpretation

A lens is a pure transformation from explicit context inputs to selected context outputs, including knowledge and tool descriptions. It has no node ownership and does not grant authority. Definitions may be composed, shared, and versioned. Resource policy and execution authority belong to the systems that enforce them. A live resource lookup belongs outside lens evaluation; its resolved snapshot becomes an explicit input.

## Self-improvement, Discord, and tool windows

Control also needs to be able to improve itself. It should operate a bit like a Borg collective. It needs Discord integration so I can chat to it there. Host it on this laptop for now, which has Nix, and keep using Codex as the backend in containers.

Tools should be available through lenses. A tool could run in a container somewhere while also being presented inside the website—an interactive window in chat, for example. Compile those interactive surfaces with Rust/WASM if possible, rather than implementing them directly in JavaScript or Nix.

Filter these ideas and incorporate the useful parts into the design. I will provide a Discord token in an `.env` file and say when it is ready. Do not expose the token or read that file directly. Keep iterating.

### Filtered direction

- **A collective, with accountable changes.** Shared rooms coordinate agents. Knowledge promotion remains explicit and evidence-backed. Control can propose changes to its own source, test them in isolated environments, and present a patch for review. Self-improvement is a normal recorded workflow; it does not give an agent permission to rewrite its own authority or deploy over the running service.
- **Local control plane, isolated workers.** The laptop hosts the Rust web service and durable local state. Codex belongs inside a provisioned worker container or microVM. Nix defines reproducible environments; a development shell alone is not the isolation boundary. Missing container infrastructure must produce a visible blocked state, never a host-execution fallback.
- **Discord is another conversation surface.** Discord messages enter the same run/room system as website messages. Limit participation to configured channels and users. The Discord adapter owns its credential; tokens never enter agent prompts, browser state, knowledge records, logs, or worker environments. Do not activate it before the operator confirms readiness.
- **One tool, separate surfaces.** A tool descriptor identifies its input/output contract, execution endpoint, required capabilities, and optional UI component. A pure lens selects descriptors. The runtime authorizes execution. A Rust/WASM component can display results and request actions through that same authorized interface; being embedded in a page does not grant extra capabilities.
- **Shared pure core.** Context selection and inspection should be the same Rust logic on the server and in compiled browser components. Nix remains infrastructure configuration, not the UI programming language. Compiler-generated browser interoperability glue is distinct from application logic.
- **Run provenance.** Pin the source snapshot, lens definition, persona, policy, worker image, and test results for an improvement run. Produce a reviewable artifact before applying or deploying it. Peer agreement is not independent evidence.

### Current boundary

The application is evolving from a local UI prototype. Nix isolation, real agent execution, model providers, judges, federated communication, and permission enforcement must be identified separately from demonstration records. Authority queue review alone never grants host capabilities.

## Container build constraint

Use Nix to build containers. Do not use bloated distribution bases such as Ubuntu. The worker image should be assembled directly from its declared Nix dependencies.

## Discord credential naming

The Discord token is supplied as `DISCORD_TOKEN` in the local `.env` file. Only the explicitly activated bridge process may load it; development tools must not inspect or print the file.

## Discord roles and identity

Not everyone who talks to Control through Discord should be an architect or be able to change its source. Allow `@geodesic` as the architect. Also allow `@vivkelly` and `@olliev` to test Control with a user role: they can use agents and spawn containers, but cannot ask Control to adapt itself.

Use stable IDs rather than raw usernames for authorization. Discord's native snowflake IDs serve this purpose; usernames are display labels, not proof of identity. Bind geodesic to `119363447514202113`, vivkelly to `251592424139390976`, and olliev to `1032836812873400380`.

### Enforcement

- Deny unknown identities and channels by default. Unbound labels grant nothing.
- Check role policy at the Discord adapter, trusted queue ingress, and worker dispatch. Do not trust a claimed role in a message, prompt, or serialized queue envelope.
- Ordinary tasks get an isolated writable user workspace without Control source, identity policy, operator session, or runtime state. A request to adapt Control requires an explicit improvement action and architect authorization.
- Only authenticated local operator access can administer the web control plane. A worker must not acquire architect access by calling the laptop's HTTP endpoints.
- Keep identity configuration outside agent workspaces. Discord commands cannot edit role bindings. Source changes still return for review; architect access does not automatically deploy them.

## Direct messages

I want to be able to just DM Control directly. Fix the integration so that direct messages work, rather than requiring a server channel.

Normal DM text should be accepted without a command prefix. Keep the same ID-based roles, reserve explicit improvement requests for the architect, reply to the original recipient, and do not broadcast private messages into shared rooms. A server channel should be optional. Bot connectivity and agent-worker readiness are separate states and should be reported honestly.

Discord installation and delivery eligibility are also separate from listener health. Successfully opening or reading a bot-side DM channel does not prove that a user can send messages to it. Provide an installation diagnostic and distinguish listening from end-to-end delivery verification.

## Server privacy

I added Control to a server, but I want to make sure it cannot see all the text in our Discord server.

Keep the integration strictly DM-only. Server membership enables the Discord relationship needed for DMs; it does not authorize ingesting server conversations. Disable server-message routing and authorization in the application, even if someone later supplies a channel ID. Separately audit effective Discord permissions using metadata only: a zero-permission invite can still inherit @everyone's channel visibility. Do not fetch server messages to test access.

## Worker credentials

Credentials should be mounted through to containers rather than injected into their environment. Mount only the required credential file read-only, separately from the writable job workspace, and deny agent tools access to it. Read-only mounting prevents modification; it does not itself prevent credential reads.

The broker is planned work. The worker adapter uses a read-only host Codex auth-file mount, an ephemeral Codex state directory, and a named permission profile denying tools access to that directory. A credential-free canary verifies filesystem isolation and the inner PID namespace before any real credentials are mounted. Authentication refresh remains host-owned because the mount cannot persist token updates.

## Tool containers and swarms

Tools themselves can be Nix containers that models work with. Control should expose an MCP interface for orchestrating agents and tools. Agents request separate tool containers as needed, rather than modifying their own sandbox. The end game is swarms with judge roles that verify results, ideally across different models so a model does not review its own work.

A tool request declares its Nix derivation or pinned image, inputs, outputs, resource limits, lifetime, and requested capabilities. Policy authorizes it; unknown packages can be submitted for an isolated Nix build and review. Agents do not receive the Docker socket or permission to choose arbitrary mounts, credentials, host paths, or networks. A pure lens selects tool descriptions; the runtime launches and stops authorized instances. Tool containers can serve MCP and a Rust/WASM window through the same authorized interface.

Judges receive task criteria, evidence, artifacts, and check results. Record author and reviewer model/provider provenance; require a different model for independent review and prefer a different provider when available. If none is configured, keep the result unverified. Agreement is not a substitute for reproducible checks, and a judge cannot grant itself deployment authority.

Track provider-reported token usage at attempt/run, person, provider/model, day, and node levels. Preserve retry costs, distinguish missing counts from zero, and keep cached tokens within input totals. Subscription usage is not an invented dollar bill. Keep website copy short and focused on actions and status; put architecture in docs or expandable details. Use local Locust for adversarial review where useful.

Chat replies should answer the request naturally. Mention changes, checks, or limitations only when useful. Greetings do not need empty report sections or a disclaimer that no application task was specified. Keep that boilerplate out of subsequent replies too, even if it appears in earlier conversation history.

### Next

- **Tool orchestration:** authenticated MCP interface, declarative Nix tool-container requests, bounded swarm scheduling and cleanup.
- **Independent verification:** separate-model judges, reproducible evidence, and usage/budget limits across the swarm.
- **Credential broker:** authenticate outside workers, with revocable per-run capabilities, usage monitoring, and enforced prevention of direct-provider bypass.

## Manage navigation

Manage is a normal top-level sidebar entry. Its less frequently visited views—Nodes, Access requests, Personas, Models & providers, Discord access, and Integrations—stay one level deep in a secondary navigation column: top-level navigation | section navigation | content. Keep the section visible while moving between those views, including older direct links. On narrow screens, use a horizontal section row above the content. Keep the main sidebar compact, scroll only its links when needed, and avoid dropdown styling or a separate top-level Inbox.

Keep management screens minimal: controls, status, and useful feedback. Discuss and explicit improvement requests let us evolve Control; pages do not need instructional architecture prose. Models & providers stores the default Codex model for new runs, captures it per run, and labels unconnected adapters honestly. Additional provider backends remain separate implementation work.

## Control harness and shared sessions

Build our own `control-harness`: a Rust TUI integrated with Control's server and MCP hive. Pi's minimal, pluggable shape is inspiration; Pi and its Node/TypeScript runtime are not dependencies. Keep clients thin. Providers, tools, policy, context, and scheduling belong behind the server interface, so a session can move between the terminal, Discord, and a browser.

Discord should reply with a local chat link that opens the same conversation on the website. A link identifies a session; it is not an access token. Continuations retain the original actor's authority, even when the local operator opens a Discord user's chat. The first harness provides listing, creating, resuming, sending, and observing sessions through authenticated MCP. Codex feature parity remains a target, including streaming, steering/cancellation, resumable execution state, patch review, provider switching, and tool plugins; this first client does not claim parity.

Efficiency, reliability, and distribution are design goals. Avoid polling unchanged transcripts, preserve durable queue transitions, and keep execution separate from clients. Build analysis/profiling tools as isolated Nix containers with explicit code/input and report/output mounts; establish measurements before adding complexity.

The first analysis tool is `tools/rust-analysis`: a pinned Nix image for formatting and offline Clippy checks, with a read-only Rust source export, separate reports, and no network or credentials. It is currently a local launcher; MCP registration and sampled flamegraphs remain separate work. Profiling requires a declared workload and verified kernel support, not a blanket privileged container.

Navigation is global: agents, usage, rooms, and settings are not children of Engineering or Research. A browser remembers a default context for new chats; each chat captures its own selected lens and does not change when the operator browses elsewhere. No lens is a valid choice. Older chats with no recorded lens remain unassigned. The current demo lens registry previews tool descriptors and contributes context to worker prompts; it does not enforce a per-lens tool capability set or grant authority.

## Rooms, visible lenses, and writing

Rooms need to connect to chats and be available as tools/MCP/skills so agents can use them. Lenses should stay visible in the sidebar, with a clear meaning, and the chat selector should use direct choices instead of an awkward dropdown. Clarify whether the editor is for knowledge-base documents.

A room links explicitly to chats. Its agents can read shared updates and post concise findings through room-scoped MCP tools; the host binds identity and room membership. Room messages are untrusted context, not instructions from an architect. Private transcripts are not broadcast. Room updates do not automatically spawn work or become knowledge. The first implementation uses a credential-free Rust stdio transport in the Nix worker image and host-validated workspace messages, preserving disabled tool networking and original actor authority.

Lenses are pure selections of context and tool descriptions, presented in a visible library and as choices for new chats. Global navigation stays global. The existing editor is a saved scratchpad, not a knowledge document editor or a write-through view of DESIGN.md; label it accordingly. Knowledge remains a separate, evidence-backed review flow.

The saved-draft action needs obvious feedback. Bring the editor into Knowledge rather than keeping a separate tab: show a list of documents, a [+] action, and a web of connections like Obsidian. Preserve the existing draft as Scratchpad.md. Connections come from explicit [[note links]], with backlinks and unresolved references visible. Saving confirms the document and revision after persistence; stale tabs cannot silently overwrite newer edits. Writing notes and accepting evidence-backed claims remain distinct actions within the same workspace. The first document store stays local to Control's state file; a filesystem vault and sync are not implied.

Keep a single Chat tab rather than a chat panel on every page. Architects can open a design discussion from a small link on the relevant page, carrying the page topic without sending a message or starting work automatically. Lenses need one compact menu item, not another stack at the bottom of the sidebar. Each chat selects lenses, and lenses compose: the UI must allow combinations instead of treating them as mutually exclusive modes. The first additive composition unions selections and deduplicates shared tool descriptors; it grants no authority. Changes apply to future messages, with the composed set captured per run.

## Lens library and conversation layout

Engineering and Research should be prebuilt compositions of smaller useful lenses, rather than two special cases. Build the lens library. Chat should resemble a traditional conversation, with tags at the bottom rather than an extra control bar above the messages.

The first library represents a lens as a declarative pure context function: component references plus selected knowledge documents, tool descriptors, and instructions. Components can themselves be compositions. Resolve against explicit document snapshots; reject cycles and missing references, deduplicate outputs, and preserve definition revisions. Engineering and Research use the same mechanism as user-created lenses. Preview the resolved result before use, and retain the resolved definitions, instructions, and document contents/revisions with each run. Editing a live lens or document affects future runs. Definitions and their revision history are local, with JSON export; distributed publication and synchronization remain future work. A selection cannot grant authority, and complete runtime tool filtering remains separate work.

Chat presents active lenses as composer tags, with additional choices revealed on demand. Submit lens changes with the next message so choosing context does not discard the draft. Keep transcript refresh independent of the composer and make that frame visually continuous with the conversation.

Keep chat compact and minimal: text-fitting user bubbles, tight paragraph spacing, and small square corners rather than pill or squircle shapes. Use the same geometry for the composer and lens tags, retain the monospace typography and readable text size, and keep the composer visible on phone screens.

Conversations open from a compact button at the top-left of the chat, closed by default. Expand a left navigation panel with New chat and the current conversation highlighted; on narrow screens it overlays the chat. Keep the opposite side available for an Artifacts button when actual outputs exist. Omit repetitive sender/status labels on completed replies; show progress or failures only when useful.

## Distribution, node operators, and persistent agents

Control is Nix, a collection of Nix-built containers, an MCP server, lenses, chats, a harness, and agent swarm managers: something like a federated, AI-native Kubernetes. Each node has operators and agents. Agents can run persistently with personas and manage other agents. Package Control as a single distributable binary so other people can run nodes and link them together. Review the code's security and prioritize making Control usable.

The single-binary target is one executable per platform with server, TUI, MCP, and node-management commands, embedded UI assets, and explicit local configuration/state paths. Worker environments remain separately pinned Nix artifacts running under a compatible container runtime. The first shared CLI entry point is `control serve` / `control tui`; complete standalone packaging is not yet implemented.

Federation should connect independently administered nodes, not make every peer a local operator. Use authenticated node identities, explicit pairing, and scoped, revocable delegation. Resolve remote resources into explicit snapshots before pure lens evaluation. Share selected lens/document revisions and room events deliberately; do not replicate private chats or credentials by default. Start with two paired nodes exchanging an explicitly shared resource, then add delegated jobs with durable IDs, deadlines, budgets, and local authorization. Persistent agents should be supervised services with durable state, restart rules, and bounded delegation; model processes need not remain alive indefinitely.

Local readiness comes first: hard per-run storage bounds and crash/restart worker reconciliation are concrete gaps identified by the independent review. Agent-management permissions, model capabilities, and node operator authority must remain distinct. Peer pairing and a network tunnel do not themselves grant execution or source-editing authority.

## Federation specification and harness refinement

The terminal should feel like one chat window, closer to Pi or Codex. Use the website's muted dark palette, a grey-purple input area with a `>` prompt, ordinary title case, and compact keyboard menus rather than three permanent boxes.

Write a federation spec using ActivityPub and Bluesky/AT Protocol as theoretical foundations. Have two agents challenge and revise it, starting in a temporary document, then commit the spec and implement a concrete slice. Agents can propose how to schedule other agents and Nix containers through declarative workflows. Keep deterministic work in code: do not spend tokens on queue eligibility, state transitions, authorization, retry timing, or process cleanup. Balance reliable computer-science fundamentals against unnecessary framework bloat.

The agreed [specification](docs/FEDERATION.md) starts with signed, recipient-bound, closed lens bundles and explicit directional trust. Imports remain quarantined. Live delegated execution must wait for enforced disk quotas and crash-safe worker reconciliation. Persistent agent managers propose plans and judge semantic results; a small deterministic supervisor owns enforcement.

## Browser containers and using Control from Codex

Headless browsers should be tools in Nix containers. Start with offline rendering
and screenshots of explicitly selected HTML previews; keep profiles ephemeral and
return bounded artifacts. Use software rendering for ordinary UI work. GPU access
should be an optional capability with verified runtime support, not a default
privilege grant. Networked browsing and interactive automation are separate steps.

Use Control's own tools while building it. A native Rust stdio MCP bridge lets the
host Codex read lenses, rooms and chats, and capture offline previews. Keep the
operator credential inside the trusted bridge, out of configuration and browser
containers. Worker agents retain their existing scoped tool access; they do not
inherit the host operator's bridge. The browser runner, inputs, limits and setup
are documented in [tools/browser](tools/browser/README.md).

Expose Browser under Tools with a small capture form and a protected screenshot
preview. Keep its current offline HTML scope explicit.

## Agent state, attempts, and live activity

Agents and their containers need a clear separation of concerns. A container can
die or an agent can break its environment without losing the conversation. Keep
chat sessions, objectives, captured context, attempts, and results in the control
plane. Retries should create fresh environments and preserve previous evidence.
Show the current command and its output in both the web chat and terminal,
together with context-compaction activity when it happens.

Current implementation: each attempt uses a fresh ephemeral Codex process and a
separate host-backed workspace. Retry is explicit; it does not resume the previous
process or automatically replay side effects. Startup marks in-flight runs
interrupted. Reconciling orphan containers and resumable provider sessions remain
work before automatic recovery or delegated execution is safe.

Persist a bounded recent activity log per attempt, from structured provider events,
excluding reasoning and arbitrary event payloads. Commands and reported output
are untrusted text: escape HTML and strip terminal control characters. Web chat
refreshes its transcript independently of the composer; the TUI uses chat revision
updates. Output arrives at provider event granularity, not necessarily per stdout
chunk. The current Codex exec event schema does not expose compaction notifications;
surfacing backend compaction needs a richer adapter and must not be inferred from
tool activity.

The current 20,000-character history window is trimming, not semantic compaction.
Record the number of earlier exchanges omitted for each attempt while retaining
the full saved conversation. Durable summaries with provenance and an inspectable
before/after boundary remain future work. Bound each attempt to 64 recent activity
items and 4,000 output characters per item; this is progress evidence rather than
a complete command audit log. Activity is visible only through authenticated local
operator chat/MCP access and never automatically forwarded to Discord or rooms.

## Everyday tools and the next backend boundary

Add useful defaults: public web search using a key-free provider such as
DuckDuckGo or Bing, a Rust playground for small programs, and modern `rg`/`fd`
commands in the Nix development environment and worker image. Playground runs
should use fresh credential-free, offline containers with bounded resources and
output, rather than changing an agent's environment.

Tools should be one searchable catalogue, not repeated under “lens tools” and
“selected tools.” Search accepts ordinary tool names and `lens:engineering`
filters (also accept `lense:`). Multiple lens filters compose their tool selections;
text terms narrow the result. Resolve saved compositions, not hard-coded categories.
Avoid lens-tag piles and duplicated previews.

Snapshots are next. Have an agent review arXiv:2608.25512 for possible lessons,
without making code changes from the paper. Investigate Claude integration behind
Control's harness and MCP rather than depending on Claude's own tools. API keys
and subagents should be supported in future; use another independent review to
find small, justified abstractions. Keep the todo short. Findings and limitations
are recorded in [research notes](docs/RESEARCH-NOTES.md).

## An augmentable collective construction tool

I like some of the ideas in [Pi Durable](https://earendil.com/posts/pi-durable/),
but we are not using Node. We are trying to go beyond it. Control is a harness,
an MCP server, a NixOS configuration, and potentially a model-calling server or
router: parts of a new, augmentable tool that anyone can run and improve with Nix.

Federation provides interoperability and social-style connections between nodes.
Control should still be useful to an individual on its own: a general-purpose
constructor for code. Nix, functional programming, and Rust are fundamental, so
we avoid whole classes of implementation issues and make the system easier to
reason about.

Imagine packaging Control with a model and a Nix configuration, ready to deploy
on suitable infrastructure and join a collective. Anyone can extend their own
installation; that does not make them an architect or administrator of someone
else's node.

Have an agent review the post, compare goals, and identify ideas worth borrowing.
For now this is architecture and investigation, not a request to implement changes
from the post. The consolidated direction and implementation boundaries are in
[Control architecture](docs/ARCHITECTURE.md).
