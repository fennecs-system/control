# Bot and browser inspiration

Ideas from other agent products, kept as input for the
[roadmap](SWARM-ROADMAP.md). Product descriptions are vendor claims, not
independently verified. "Grok Bot" is xAI's; "Muse" is Meta's personal agent.

| Source | Useful idea | Control interpretation | Status |
| --- | --- | --- | --- |
| [OpenClaw sub-agents](https://docs.openclaw.ai/tools/subagents) and [scheduled jobs](https://docs.openclaw.ai/automation/cron-jobs) | Separate child sessions, bounded nesting, durable scheduling. | One supervisor; children get narrower grants and budgets; scheduling stays deterministic. | Children shipped ([delegation](DELEGATION.md)); schedules are roadmap W1. |
| [Grok Bot](https://docs.x.ai/grok-bot/overview) and [routines](https://docs.x.ai/grok-bot/skills-routines-and-automations) | Persistent named collaborators; inspectable, triggered workflows; an operator credential vault. | Bot identity and configuration persist apart from replaceable runs; small declarative routines are versioned and validated before activation. | Persistent bots ([bot = agent + supervisor](TURN-CONTINUITY.md#bot--agent--supervisor--callbacks)) and agent-written [routines](WATCHES.md#routines) shipped; validated declarative recipes are W2; the vault is V1. |
| [Muse](https://about.fb.com/news/2026/09/introducing-muse-personal-ai-agent/) | Isolated execution, protected credentials, a separate gate for sensitive outbound actions. | Effect authorization lives in the broker. A model judge can supply evidence but never grants permission. | Design principle; see [threat model](THREAT-MODEL.md). |
| [OpenClaw browser](https://docs.openclaw.ai/tools/browser) and [manual login](https://docs.openclaw.ai/tools/browser-login) | A separate browser profile, observe/act tools, human handoff. | The browser is a granted resource with bounded lifetime, downloads and network access; personal profiles and operator sessions stay outside it. | Idea; today only offline capture exists (roadmap U2). |

## Browser automation

Use a Rust client over a language-neutral protocol such as
[WebDriver](https://www.w3.org/TR/webdriver2/) or
[WebDriver BiDi](https://www.w3.org/TR/webdriver-bidi/), after checking real driver
support. Agent-authored automation can stay programmable without making Node part
of Control. An unrestricted debugging endpoint would bypass mediated action
checks, so do not claim action enforcement while exposing one.

## Candidate workflows

- Watch a selected release source, prepare an isolated patch, run checks and ask
  for review.
- A travel watcher collects offers and asks about dates. Booking needs
  authorization tied to the exact itinerary and price.

Missing replies and ambiguous remote outcomes never become consent or automatic
retries. Nix reproducibility does not replace sandboxing: keep images immutable,
writable package state private and host daemon sockets unavailable.
