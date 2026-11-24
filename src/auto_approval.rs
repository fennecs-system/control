//! Automatic approval of routine agent requests (Manage → Permissions).
//!
//! Requests still go through the ordinary approval paths: an agent files a
//! pending port or bot request, and when the node's policy approves it the
//! host makes the same transition the operator's form would, at once. The
//! decision is recorded with the policy as decider and the rule that matched
//! (`State::auto_decisions`, shown on the Permissions page), and an optional
//! rate-limited FYI notice is forwarded through the operator channels. An
//! auto-approved port or bot is revoked the same way as a manual one.
//!
//! Deciding is a pipeline of [`Classifier`] stages over plain facts:
//!
//! 1. [`Rules`], always first. It returns `Approve` for routine requests,
//!    `Human` for what must never be automatic (strict mode or principal,
//!    unlisted private targets, exhausted limits) and `Ask` for the
//!    borderline rest. Folder and credential requests never enter it.
//! 2. An optional later stage, such as a model-based classifier, may only see
//!    `Ask` verdicts in `Mode::Auto`, and may only turn them into `Approve`.
//!    It can never override `Human`. None is installed today: [`classify`]
//!    passes no later stage, so `Ask` waits for the operator.
//!
//! Gathering facts (`ports::auto_approve`, `bots::auto_decide`) and applying
//! a verdict stay in each domain; this module performs no I/O.
use crate::{State, identity::Actor};
use control::permissions::{AutoApproval, KindMode, Mode};
use serde::{Deserialize, Serialize};

/// Automatic decisions kept for the Permissions page.
const RECORDS: usize = 32;
/// Shown under "Auto-approved recently".
const SHOWN: usize = 10;
/// Persistent bots one task tree may hold through automatic approval.
pub(crate) const BOTS_PER_TASK: usize = 8;
/// Persistent bots the node may run before bot requests ask again.
pub(crate) const BOTS_PER_NODE: usize = 32;
/// Active ports one task tree may hold through automatic approval: one
/// chat's own quota, shared by the whole tree.
pub(crate) const TASK_PORTS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Approve now; the rule that matched.
    Approve(&'static str),
    /// Borderline: waits for the operator unless a later stage approves it.
    Ask(&'static str),
    /// Never automatic: waits for the operator whatever later stages say.
    Human(&'static str),
}

/// What a classifier sees: facts the host gathered, never model text. Only
/// ports and bots exist here. Folder access (host files) and anything
/// credential-related have no variant, so no stage, today's or a later
/// model, can ever see or approve them; they always wait for the operator.
pub(crate) enum Request<'a> {
    Port(PortFacts<'a>),
    Bot(BotFacts<'a>),
}
pub(crate) struct PortFacts<'a> {
    /// Who the task acts for.
    pub actor: &'a Actor,
    pub ingress: bool,
    pub service_port: u16,
    /// Egress target, `None` for ingress and peer routes.
    pub target: Option<(&'a str, u16)>,
    /// For a peer route: whether the peer ingress is approved and its service
    /// belongs to the same task tree.
    pub peer_in_task: Option<bool>,
    /// The service is owned by a chat in the requester's task tree.
    pub service_in_task: bool,
    /// Active ports held by the task tree's services, this request included.
    pub task_ports: usize,
}
pub(crate) struct BotFacts<'a> {
    pub actor: &'a Actor,
    /// The requester's top-level chat is an authorized operator's task.
    pub operator_tree: bool,
    /// The chat would become a read-only advisor or antibody.
    pub observer: bool,
    pub task_chats: usize,
    /// The node's `max_agents_per_root`.
    pub spawn_limit: usize,
    /// Persistent bots already running in the task tree and on the node.
    pub task_bots: usize,
    pub node_bots: usize,
}

/// One decision stage.
pub(crate) trait Classifier {
    fn classify(&self, policy: &AutoApproval, request: &Request<'_>) -> Verdict;
}

/// The rule-based first stage.
pub(crate) struct Rules;
impl Classifier for Rules {
    fn classify(&self, policy: &AutoApproval, request: &Request<'_>) -> Verdict {
        let actor = match request {
            Request::Port(facts) => facts.actor,
            Request::Bot(facts) => facts.actor,
        };
        if policy.mode == Mode::Strict {
            return Verdict::Human("strict mode");
        }
        if principal(actor).is_none_or(|key| policy.strict.contains(&key)) {
            return Verdict::Human("strict principal");
        }
        match request {
            Request::Port(facts) => port(policy, facts),
            Request::Bot(facts) => bot(policy, facts),
        }
    }
}

fn port(policy: &AutoApproval, facts: &PortFacts<'_>) -> Verdict {
    if policy.port == KindMode::Ask {
        return Verdict::Human("ports set to ask");
    }
    if !facts.service_in_task {
        return Verdict::Ask("service outside the task");
    }
    if facts.task_ports > TASK_PORTS {
        return Verdict::Human("task port quota");
    }
    if facts.ingress {
        return if facts.service_port > 1024 {
            Verdict::Approve("ingress-high-port")
        } else {
            Verdict::Ask("privileged ingress port")
        };
    }
    if let Some(in_task) = facts.peer_in_task {
        return if in_task {
            Verdict::Approve("peer-route-in-task")
        } else {
            Verdict::Ask("peer outside the task")
        };
    }
    let Some((host, port)) = facts.target else {
        return Verdict::Human("no target");
    };
    if policy.standing(host, port) {
        return Verdict::Approve("standing-target");
    }
    match host.parse::<std::net::IpAddr>() {
        // Loopback, private-network, link-local and other special addresses
        // are never automatic unless listed above.
        Ok(ip) if !crate::web_access::public_ip(ip) => Verdict::Human("unlisted private target"),
        Ok(_) => Verdict::Ask("literal public address"),
        Err(_) if !public_name(host) => Verdict::Human("unlisted private target"),
        Err(_) if matches!(port, 80 | 443) => Verdict::Approve("public-web"),
        Err(_) => Verdict::Ask("unusual port"),
    }
}
/// A name that is not obviously local. Approval still pins the name to
/// public addresses only, so one that resolves privately stays pending.
fn public_name(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host.contains('.')
        && ![
            ".localhost",
            ".local",
            ".internal",
            ".lan",
            ".home.arpa",
            ".localdomain",
        ]
        .iter()
        .any(|suffix| host.ends_with(suffix))
}

fn bot(policy: &AutoApproval, facts: &BotFacts<'_>) -> Verdict {
    if policy.bot == KindMode::Ask {
        return Verdict::Human("bots set to ask");
    }
    if !facts.operator_tree {
        return Verdict::Human("not an operator task");
    }
    if facts.node_bots >= BOTS_PER_NODE {
        return Verdict::Human("bot or spawn limit");
    }
    // A read-only observer is a background helper with its own allowance
    // (`max_advisors_per_root`, checked at spawn); it never uses member capacity.
    if facts.observer {
        return Verdict::Approve("observer-in-limits");
    }
    if facts.task_chats > facts.spawn_limit || facts.task_bots >= BOTS_PER_TASK {
        return Verdict::Human("bot or spawn limit");
    }
    Verdict::Approve("bot-in-task-limits")
}

/// The policy key of the principal a task acts for; `None` never auto-approves.
pub(crate) fn principal(actor: &Actor) -> Option<String> {
    match actor {
        Actor::LocalOperator => Some("local".into()),
        Actor::DiscordDm { user_id, .. } | Actor::Discord { user_id, .. } => Some(format!("discord:{user_id}")),
        Actor::Unknown => None,
    }
}

/// The node's decision for `request`: the rules, then any later stage.
pub(crate) fn classify(policy: &AutoApproval, request: &Request<'_>) -> Verdict {
    decide(policy, request, None)
}
/// The pipeline with an optional later stage (see the module notes).
fn decide(policy: &AutoApproval, request: &Request<'_>, later: Option<&dyn Classifier>) -> Verdict {
    match (Rules.classify(policy, request), later) {
        (Verdict::Ask(reason), Some(stage)) if policy.mode == Mode::Auto => match stage.classify(policy, request) {
            Verdict::Approve(rule) => Verdict::Approve(rule),
            _ => Verdict::Ask(reason),
        },
        (verdict, _) => verdict,
    }
}

/// An automatic decision, recorded like the operator's with the policy as decider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub unix: u64,
    /// `port` or `bot`.
    pub kind: String,
    /// The port or bot request ID.
    pub id: String,
    /// The requesting chat.
    pub chat: String,
    pub what: String,
    pub rule: String,
    /// Always `policy` today; a later stage would name itself.
    pub decider: String,
}

/// Records an automatic approval and queues its FYI notice when enabled.
/// Part of the caller's checkpoint: it only edits `state`.
pub(crate) fn record(
    state: &mut State,
    policy: &AutoApproval,
    kind: &str,
    id: &str,
    chat: &str,
    what: String,
    rule: &str,
) {
    let at = crate::bots::now_ms() / 1000;
    let text = format!(
        "Auto-approved {kind}: {what} (rule {rule}). No action needed; revoke it in Control if it should not run."
    );
    state.auto_decisions.push(Record {
        unix: at,
        kind: kind.into(),
        id: id.into(),
        chat: chat.into(),
        what,
        rule: rule.into(),
        decider: "policy".into(),
    });
    let excess = state.auto_decisions.len().saturating_sub(RECORDS);
    state.auto_decisions.drain(..excess);
    if policy.notices {
        crate::intel::policy_notice(state, chat, &text, &format!("policy-{id}"), at);
    }
}
/// The rule that approved request `id`, if the policy did.
pub(crate) fn rule(decisions: &[Record], id: &str) -> Option<String> {
    decisions
        .iter()
        .rev()
        .find(|record| record.id == id)
        .map(|record| record.rule.clone())
}

fn when(unix: u64) -> String {
    format!(
        "{} {:02}:{:02} UTC",
        crate::intel::date(unix),
        unix % 86_400 / 3600,
        unix % 3600 / 60
    )
}
/// "Running now" and "Auto-approved recently" for the Permissions page:
/// live services, open ports with their targets, persistent bots and the
/// latest automatic decisions, copied out of state to render as compact lines.
pub(crate) struct Overview {
    services: Vec<Service>,
    ports: Vec<crate::ports::Open>,
    bots: Vec<Link>,
    /// The latest first.
    decisions: Vec<(Record, Link)>,
}
/// A live service's line.
struct Service {
    id: String,
    phase: &'static str,
    label: String,
    owner: Link,
}
/// A chat by the name the page shows.
struct Link {
    chat: String,
    name: String,
}
impl Link {
    fn of(state: &State, chat: &str) -> Self {
        Self {
            chat: chat.to_owned(),
            name: crate::callsigns::name_or_id(&state.chats, chat).into_owned(),
        }
    }
    fn render(&self) -> String {
        format!(
            "<a href='/dm/{}'>{}</a>",
            crate::escape(&self.chat),
            crate::escape(&self.name)
        )
    }
}
impl Overview {
    pub(crate) fn of(state: &State) -> Self {
        Self {
            services: state
                .services
                .iter()
                .filter(|service| service.live())
                .map(|service| Service {
                    id: service.resource.id.as_str().to_owned(),
                    phase: service.phase_name(),
                    label: service.label(),
                    owner: Link::of(state, service.resource.owner.chat.as_str()),
                })
                .collect(),
            ports: crate::ports::overview(&state.ports, &state.auto_decisions),
            bots: crate::bots::active(state)
                .into_iter()
                .map(|chat| Link::of(state, chat))
                .collect(),
            decisions: state
                .auto_decisions
                .iter()
                .rev()
                .take(SHOWN)
                .map(|record| (record.clone(), Link::of(state, &record.chat)))
                .collect(),
        }
    }
    pub(crate) fn render(&self) -> String {
        let services = self
            .services
            .iter()
            .map(|service| {
                format!(
                    "<p>{} · {} · {} · {}</p>",
                    crate::escape(&service.id),
                    service.phase,
                    crate::escape(&service.label),
                    service.owner.render()
                )
            })
            .collect::<Vec<_>>();
        let ports = self.ports.iter().map(crate::ports::Open::render).collect::<Vec<_>>();
        let bots = self
            .bots
            .iter()
            .map(|chat| format!("<p>{}</p>", chat.render()))
            .collect::<Vec<_>>();
        let lines = |items: &[String], empty: &str| {
            if items.is_empty() {
                format!("<p class='muted'>{empty}</p>")
            } else {
                items.concat()
            }
        };
        let decisions = self
            .decisions
            .iter()
            .map(|(record, chat)| {
                format!(
                    "<p><span class='tag'>{}</span> {} · rule {} · {} · {}</p>",
                    crate::escape(&record.kind),
                    crate::escape(&record.what),
                    crate::escape(&record.rule),
                    chat.render(),
                    when(record.unix)
                )
            })
            .collect::<Vec<_>>();
        format!(
            "<h3 class='form-title'>Running now</h3><div class='metrics'><div><b>{}</b><span>SERVICES</span></div><div><b>{}</b><span>OPEN PORTS</span></div><div><b>{}</b><span>BOTS</span></div></div>\
            <article class='record'><h3>Services</h3>{}</article><article class='record'><h3>Ports</h3>{}<p class='muted'><a href='/tools/ports'>Revoke on the TCP ports page ↗</a></p></article>\
            <article class='record'><h3>Bots</h3>{}<p class='muted'>Stop a bot with /bot off in its DM.</p></article>\
            <h3 class='form-title'>Auto-approved recently</h3>{}",
            services.len(),
            ports.len(),
            bots.len(),
            lines(&services, "No live services."),
            lines(&ports, "No open ports."),
            lines(&bots, "No persistent bots."),
            if decisions.is_empty() {
                "<div class='empty'>No automatic decisions yet.</div>".to_owned()
            } else {
                format!("<article class='record'>{}</article>", decisions.concat())
            }
        )
    }
}

#[cfg(test)]
mod tests;
