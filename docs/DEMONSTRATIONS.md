# Demonstration goals

Planned acceptance scenarios, 2026-10-30. These should demonstrate breadth using
the same tools, resource grants, lenses, personas and supervisor as normal work.
They do not add a website builder, price-tracker subsystem or game scheduler to
Control's core. See the [roadmap](../roadmap.md) for priorities and open gates.

Control and authored integrations remain Rust/Haskell/Lean/WASM/Nix. Non-Rust
third-party tools may run only inside tool or agent containers, with pinned
dependencies and explicit resource/network grants. A Rust adapter can call an
isolated upstream tool; no authored JavaScript integration or host Node service.
Keep base images immutable and generated tools in private writable state.

## Foundation: Control improves Control

Use `/improve` for a small real change: discuss, edit isolated source, invoke a
declared build/test environment, follow long-running command output and retain
verification alongside a reviewable diff. Review and application to the checkout
remain explicit. Repeat with a follow-up and a server restart before relying on
this loop for larger work. This is the first practical target, not a fifth
special-purpose framework.

## 1. Build a non-trivial website

Start with a basic page to exercise the tools before the full scenario:

> Build a small three-page reading-list website in Rust and HTML with inline
> CSS, mono type and the purple Control palette. Save the source, compile and
> test the Rust server using the workspace command tools, then start a local
> preview. Use control_browser_capture on the self-contained HTML, inspect the
> screenshot and revise any layout problems. Give me the preview and screenshot
> links and a concise account of what was tested. Use no remote assets or host
> commands.

The worker capture accepts up to 16,000 bytes of HTML and uses the pinned offline
Firefox image. It visually checks that HTML; it does not navigate the running
preview. Rust tests or HTTP checks inside the sandbox cover server behavior.
Use the normal project/artifact checkpoint and preview tools rather than adding a
website builder to Control. This basic flow needs no internet grant; public-page
research can use [scoped HTTP access](WEB-ACCESS.md) when explicitly approved.
It is a first exercise, not completion of the persisted-data scenario below.

An agent builds a useful website with multiple routes/views, persisted data,
input validation and tests. The owner opens a running preview, requests a
meaningful change and inspects the resulting diff and verification. Reconnect to
the chat and restart Control without losing source or the published checkpoint.
Verify actual behavior with the browser tool, not only a successful compilation.

Needed capabilities: a reproducible project build environment with dependencies,
commands longer than the current short Claude shell deadline, usable logs and
browser checks. Current preview mounts are read-only and temporary files vanish
with the container; persistent application data needs a separately scoped resource.
Do not quietly make the source or host filesystem writable to solve that need.

## 2. Browser agents and a phone-price history tool

Give each agent a different authorized website and a comparable phone variant.
Agents use a real browser session to observe offers, and build a tool with a
Rust/WASM view reachable inside Control. It shows price over time and links each
point back to its source. This exercises organized collaboration, structured data
and a tool-provided interactive view; it is not another hardcoded dashboard tab.

- Start with controlled browser fixtures, then a small set of public sites.
  Agents can propose extraction code within their isolated tool environments.
- Store observations as structured data: source URL, observation time, product
  variant, currency, amount, availability and bounded supporting evidence. Keep
  missing values, failed visits and non-comparable offers explicit; no invented
  prices or silent conversion between variants/currencies.
- Run at least two collection rounds and show each site's independent results.
  Retry/restart must not duplicate an acknowledged observation. Stored captures
  can verify extraction and rendering without repeatedly calling live websites.
- Keep routine scheduling, validation and aggregation deterministic. Use agents
  for website differences and changed layouts; do not require model calls for
  every unchanged price sample. Results belong in a dataset, not a growing pile
  of prose in the shared knowledge base.
- Publish the generated tool/view through the normal tool and artifact/resource
  contracts, with the appropriate browser-origin isolation and bounded output.

Gaps: interactive network-granted browser sessions, durable collection triggers,
a shared observation store, and a
versioned tool/view publication contract. Demonstrate manual collection rounds
before introducing unattended schedules.

## 3. Cooperating agents in Minecraft

An agent starts a server in a tool container and publishes its approved TCP port.
Another agent creates a compatible client inside its own tool container; the
owner connects too. Scale from one client to a small team, using room updates,
admitted knowledge and independent model turns. Connections outlive model turns;
cancellation and grant revocation close the affected resources.

The target demonstration uses an offline-mode Minecraft server and five agents
with Mineflayer clients. Agents create their own adapters and share useful tools
and knowledge. These are user-created workloads outside this repository; the
Control implementation remains generic.

For the next playtest, request **Minecraft Java 26.3** explicitly. Resolve that
version from Mojang's manifest, verify its download hash and required Java runtime,
and verify client protocol support before starting. Mojang's 26.3 metadata requires
Java 25: provision that runtime in the private service if the image's default Java
is older. Mineflayer's published support
list currently ends at 26.1; use a pinned ViaProxy release supporting 26.3 between
the bot and server, with the bot using the supported 26.1 protocol. The proxy is
a task-specific tool-container workload, not a Control integration. Do not silently
choose 1.21.1. See the [Mineflayer discussion](https://github.com/PrismarineJS/mineflayer/discussions/4190).

Suggested prompt:

> Run a local offline-mode Minecraft Java 26.3 server and one persistent Mineflayer
> bot named Cedar in separate persistent full-worker services, with ViaProxy
> translating Cedar's supported 26.1 protocol to the 26.3 server. Provision Java 25
> in the private service if needed. Verify runtime and
> dependency requirements, and pin and verify downloads. Use approved relays for
> dependencies. Request server ingress and each inter-service egress using
> peer_port_id; connect each client to its own container's loopback relay.
> Wait for approvals and readiness. Verify Cedar actually spawns and the server
> lists Cedar before attempting a house. Keep the services alive between turns
> and post concise progress in the linked room.

Version references: [Mojang release notes](https://www.minecraft.net/en-us/article/minecraft-java-edition-26-3)
and [Mineflayer's supported versions](https://github.com/PrismarineJS/mineflayer/blob/master/lib/version.js).

Server packages, protocol libraries and agent-written adapters belong to the
user's tool environments, not Control's source or a domain-specific scheduler.
Generated Node/Python programs may execute inside explicitly granted containers;
Control and its maintained integrations remain Rust/WASM/Nix. Runtime provisioning
and persistent bot identity remain separate from port access and task spawning.

## 4. The owner joins and steers the team

The owner joins the same world and asks the team to build a castle. Agents can
ask whether to use stone brick or cobblestone, then continue with the answer.

An agent asks a consequential design question through the existing durable
question service and confirmed Discord route. The owner answers that exact task,
steers its plan and lets peers continue. Test delayed/duplicate delivery and a
bridge/server restart. External-environment messages cannot impersonate an
operator or grant authority. Parent-chat question aggregation remains open.
