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

### TODO: credential broker / proxy

- Move provider authentication into a trusted broker outside agent containers, so workers receive no provider credentials or mounted credential files.
- Give each run a short-lived, revocable broker capability scoped by its authorized provider, model, tools, and usage limits. A lens selects available tools; runtime policy enforces access.
- Route provider requests through the broker and record usage, latency, cost where available, and failures without logging credentials or conversation content by default.
- Prevent workers from bypassing the broker through direct provider access. Verify isolation, expiry, revocation, and cross-run access before treating this boundary as enforced.

The broker is planned work. The worker adapter uses a read-only host Codex auth-file mount, an ephemeral Codex state directory, and a named permission profile denying tools access to that directory. A credential-free canary verifies filesystem isolation and the inner PID namespace before any real credentials are mounted. Authentication refresh remains host-owned because the mount cannot persist token updates.
