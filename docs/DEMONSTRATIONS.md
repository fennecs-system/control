# Demonstration goals

Acceptance scenarios that show breadth using the same tools, grants, lenses and
supervisor as normal work. None of them adds a website builder, price tracker or
game scheduler to Control. Priorities live in the [roadmap](SWARM-ROADMAP.md).

Control and its maintained integrations stay Rust/WASM/Nix. Third-party
non-Rust tools and agent-written programs run only inside granted tool or agent
containers, with pinned dependencies and explicit network/resource grants.

## Foundation: Control improves Control

Use `/improve` for a small real change: discuss it, edit isolated source, run a
declared [build environment](BUILD-ENVIRONMENTS.md), follow long-running
[command output](COMMAND-JOBS.md) and keep the verification beside a reviewable
diff. Review and applying it to the checkout stay explicit. The
[GitHub tool](GITHUB.md) can publish the result as a draft pull request.

## 1. Build a non-trivial website

A first exercise:

> Build a small three-page reading-list website in Rust and HTML with inline
> CSS, mono type and the purple Control palette. Save the source, compile and
> test the Rust server using the workspace command tools, then start a local
> preview. Use control_browser_capture on the self-contained HTML, inspect the
> screenshot and revise any layout problems. Give me the preview and screenshot
> links and a concise account of what was tested. Use no remote assets or host
> commands.

`control_browser_capture` renders up to 16,000 bytes of self-contained HTML in
the pinned offline Firefox image. It does not navigate the running preview; Rust
tests or HTTP checks inside the sandbox cover server behavior. Public-page
research can use [scoped HTTP access](WEB-ACCESS.md) when approved.

The full scenario: an agent builds a site with several routes, persisted data,
input validation and tests. The owner opens the running preview, requests a
change and inspects the diff and verification, then reconnects and restarts
Control without losing source or the published checkpoint. Preview mounts are
read-only; persistent application data belongs in a
[persistent service](PERSISTENT-TOOLS.md), never a writable source or host
filesystem.

**Gap:** an interactive browser that exercises the running preview (roadmap U2).

## 2. Browser agents and a phone-price history

Each agent watches a different authorized website for a comparable phone
variant, using a real browser session. Together they build a Rust/WASM view,
reachable inside Control, that charts price over time and links each point to
its source.

- Start with controlled browser fixtures, then a few public sites.
- Store structured observations: source URL, time, variant, currency, amount,
  availability and bounded evidence. Keep missing values, failed visits and
  non-comparable offers explicit; never invent prices or silently convert
  variants or currencies.
- Run at least two collection rounds. Retry or restart must not duplicate an
  acknowledged observation. Stored captures can verify extraction without
  calling live sites again.
- Keep scheduling, validation and aggregation deterministic. Use agents for site
  differences and layout changes, not for every unchanged sample. Results belong
  in a dataset, not in the shared knowledge base.
- Publish the view through the normal tool and artifact contracts, with
  browser-origin isolation and bounded output.

**Gaps:** interactive network-granted browser sessions (U2), durable collection
schedules (W1), a shared observation store and a versioned tool-view publication
contract. Demonstrate manual rounds before unattended schedules.

## 3. Cooperating agents in Minecraft

Demonstrated. Agents run an offline-mode server and Mineflayer clients in
persistent services, connect through approved [ports](PORTS.md), coordinate in
rooms and keep working across turns; a team progressed through building and
farming to diamonds. The client lives as a reusable
[tool package](TOOL-PACKAGES.md), outside Control's source. Version and protocol
details belong to that package, not to Control.

## 4. The owner joins and steers the team

The owner joins the same world and asks for a castle. An agent asks a
consequential choice (stone brick or cobblestone?) through `control_design_ask`;
the question reaches the operator in the UI or Discord. The owner answers that
exact task, steers its plan and the peers continue.

**Acceptance still to run:** delayed and duplicate answer delivery, and a bridge
or server restart while a question is pending. Messages from the game world can
neither impersonate the operator nor grant authority.
