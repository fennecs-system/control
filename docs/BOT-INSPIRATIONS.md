# Bot and browser inspiration

Reviewed 2026-10-30 for future roadmap input; these are design recommendations,
not implemented Control features. Product descriptions are vendor claims, not
independent security verification. “Grokbot” and “Muse” are interpreted here as
xAI's Grok Bot and Meta's personal agent.

| Source | Useful idea | Control interpretation |
| --- | --- | --- |
| [OpenClaw sub-agents](https://docs.openclaw.ai/tools/subagents) and [scheduled jobs](https://docs.openclaw.ai/automation/cron-jobs) | Separate child sessions, bounded nesting and durable scheduling/delivery. | Reuse one supervisor. Give children narrower grants and budgets; keep scheduling deterministic. |
| [Grok Bot](https://docs.x.ai/grok-bot/overview) and [routines](https://docs.x.ai/grok-bot/skills-routines-and-automations) | Persistent named collaborators and inspectable reusable workflows with triggers. | Persist bot identity/configuration separately from replaceable runs; version small declarative callbacks and test them before activation. |
| [Muse](https://about.fb.com/news/2026/09/introducing-muse-personal-ai-agent/) | Isolated execution, protected credentials and a separate boundary for sensitive outbound actions. | Put effect authorization in the broker. A model judge can supply evidence but cannot grant permission. |
| [OpenClaw browser](https://docs.openclaw.ai/tools/browser) and [manual login](https://docs.openclaw.ai/tools/browser-login) | A separate browser profile, observation/action tools and human handoff. | Treat the browser as a granted resource with bounded lifetime, downloads and network access. Keep personal profiles and operator sessions outside it. |

Use a Rust client over a suitable language-neutral browser protocol, such as
[WebDriver](https://www.w3.org/TR/webdriver2/) or
[WebDriver BiDi](https://www.w3.org/TR/webdriver-bidi/), after checking actual driver
support. Agent-authored automation can be programmable without making Node part
of Control. Exposing an unrestricted debugging endpoint would bypass mediated
action checks; do not claim transaction enforcement while offering that bypass.

The next concrete workflow should watch an explicitly selected release source,
prepare an isolated patch, run checks and ask for review. A travel watcher can
collect offers and ask about dates; booking needs authorization tied to the
concrete itinerary and price. Missing replies and ambiguous remote outcomes must
never become consent or automatic retries.

Finish storage/recovery and malicious-input tests before unattended execution.
Nix reproducibility does not replace sandboxing: keep images immutable, writable
package state private, shared-cache publication privileged, and host daemon
sockets unavailable. Existing `/spawn` and durable questions are foundations,
not completed persistent-bot orchestration. See the [roadmap](../roadmap.md).
