use crate::escape;
use std::collections::HashMap;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Collective {
    pub posts: Vec<crate::rooms::Post>,
    pub room_watches: Vec<crate::rooms::Watch>,
    pub(crate) proposals: Vec<Knowledge>,
    requests: Vec<Request>,
    pub notice: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) claims: Vec<crate::rooms::claims::Claim>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) claim_seq: u64,
}
fn is_zero(value: &u64) -> bool {
    *value == 0
}
crate::application::status_text::status_enum! {
    /// A knowledge proposal's review state, saved as its lowercase name.
    pub(crate) enum ProposalStatus {
        Pending = "pending",
        Accepted = "accepted",
        Rejected = "rejected",
    }
}
impl ProposalStatus {
    /// An operator's review outcome, as opposed to a state a review starts from.
    pub fn is_decision(&self) -> bool {
        match self {
            Self::Accepted | Self::Rejected => true,
            Self::Pending | Self::Unrecognised(_) => false,
        }
    }
    /// Not rejected: the claim still blocks a duplicate.
    pub fn is_standing(&self) -> bool {
        match self {
            Self::Rejected => false,
            Self::Pending | Self::Accepted | Self::Unrecognised(_) => true,
        }
    }
}
crate::application::status_text::status_enum! {
    /// An authority request's state, saved as its lowercase name.
    enum RequestStatus {
        Pending = "pending",
        Approved = "approved",
        Denied = "denied",
    }
}
impl RequestStatus {
    /// An operator's decision, as opposed to a state a decision starts from.
    fn is_decision(&self) -> bool {
        match self {
            Self::Approved | Self::Denied => true,
            Self::Pending | Self::Unrecognised(_) => false,
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Knowledge {
    pub claim: String,
    pub evidence: String,
    pub scope: String,
    pub validity: String,
    pub status: ProposalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::knowledge_tools::Source>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Request {
    capability: String,
    resource: String,
    purpose: String,
    authority: String,
    status: RequestStatus,
}

fn value(f: &HashMap<String, String>, key: &str) -> String {
    f.get(key).map(|s| s.trim().to_owned()).unwrap_or_default()
}
fn input(name: &str, label: &str, limit: usize) -> String {
    format!("<label>{label}<input name='{name}' required maxlength='{limit}'></label>")
}
fn heading(kicker: &str, title: &str, description: &str) -> String {
    let kicker = if kicker.eq_ignore_ascii_case(title) {
        String::new()
    } else {
        format!("<span class='eyebrow'>{kicker}</span>")
    };
    format!("{kicker}<h2>{title}</h2><p class='muted'>{description}</p>")
}
impl Collective {
    pub fn action(&mut self, path: &str, f: &HashMap<String, String>) -> &'static str {
        self.notice.clear();
        match path {
            "/room" => {
                let text = value(f, "message");
                let channel = value(f, "channel");
                if !text.is_empty() && text.chars().count() <= 2000 && crate::rooms::valid(&channel) {
                    self.posts.push(crate::rooms::Post {
                        room: channel,
                        text,
                        chat: None,
                        run: None,
                        key: String::new(),
                        delivery: crate::rooms::Delivery::Notify,
                    });
                }
                match value(f, "channel").as_str() {
                    "research" => "/?view=rooms&channel=research",
                    "engineering" => "/?view=rooms&channel=engineering",
                    _ => "/?view=rooms",
                }
            }
            "/propose" => {
                let claim = value(f, "claim");
                let evidence = value(f, "evidence");
                let scope = value(f, "scope");
                let validity = value(f, "validity");
                if claim.is_empty()
                    || claim.chars().count() > 280
                    || evidence.is_empty()
                    || evidence.chars().count() > 1000
                    || scope.is_empty()
                    || scope.chars().count() > 300
                    || validity.is_empty()
                    || validity.chars().count() > 300
                    || self.proposals.len() >= crate::knowledge_tools::MAX_PROPOSALS
                {
                    self.notice =
                        "A proposal needs a claim of at most 280 characters, evidence, scope, and a recheck condition."
                            .into();
                } else if self
                    .proposals
                    .iter()
                    .any(|p| p.status.is_standing() && p.claim.to_lowercase() == claim.to_lowercase())
                {
                    self.notice = "That claim already exists. Review the existing record instead.".into();
                } else {
                    self.proposals.push(Knowledge {
                        claim,
                        evidence,
                        scope,
                        validity,
                        status: ProposalStatus::Pending,
                        source: None,
                    });
                }
                "/?view=knowledge"
            }
            "/review" => {
                let decision = ProposalStatus::parse(&value(f, "decision"));
                if let Some(p) = value(f, "id")
                    .parse::<usize>()
                    .ok()
                    .and_then(|id| self.proposals.get_mut(id))
                    && p.status == ProposalStatus::Pending
                    && decision.is_decision()
                {
                    p.status = decision;
                }
                "/?view=knowledge"
            }
            "/request" => {
                let capability = value(f, "capability");
                let resource = value(f, "resource");
                let purpose = value(f, "purpose");
                let authority = value(f, "authority");
                if ["network.read", "environment.package", "tool.access"].contains(&capability.as_str())
                    && ["human", "judge"].contains(&authority.as_str())
                    && !resource.is_empty()
                    && !purpose.is_empty()
                {
                    self.requests.push(Request {
                        capability,
                        resource,
                        purpose,
                        authority,
                        status: RequestStatus::Pending,
                    });
                }
                "/?view=authority"
            }
            "/decide" => {
                let decision = RequestStatus::parse(&value(f, "decision"));
                if let Some(r) = value(f, "id")
                    .parse::<usize>()
                    .ok()
                    .and_then(|id| self.requests.get_mut(id))
                    && r.status == RequestStatus::Pending
                    && decision.is_decision()
                {
                    r.status = decision;
                }
                "/?view=authority"
            }
            _ => "/",
        }
    }
    pub fn render(&self, view: &str, _channel: &str) -> Option<String> {
        self.page(view).map(|page| page.render())
    }
    /// The Knowledge review or Access requests page, copied out of state so
    /// it renders after the state lock is released.
    fn page(&self, view: &str) -> Option<Page> {
        match view {
            "knowledge-review" => Some(self.review()),
            "authority" => Some(self.access()),
            _ => None,
        }
    }
    pub(crate) fn review(&self) -> Page {
        self.with(Kind::Review(
            self.proposals
                .iter()
                .map(|p| Entry {
                    claim: p.claim.clone(),
                    evidence: p.evidence.clone(),
                    scope: p.scope.clone(),
                    validity: p.validity.clone(),
                    status: p.status.clone(),
                    source: p.source.as_ref().map(|s| (s.chat.clone(), s.run.clone())),
                })
                .collect(),
        ))
    }
    pub(crate) fn access(&self) -> Page {
        self.with(Kind::Access(self.requests.clone()))
    }
    fn with(&self, kind: Kind) -> Page {
        Page {
            kind,
            notice: self.notice.clone(),
        }
    }
}
/// A Collective page as [`Collective::page`] copies it.
pub(crate) struct Page {
    kind: Kind,
    notice: String,
}
enum Kind {
    Review(Vec<Entry>),
    Access(Vec<Request>),
}
/// A knowledge proposal as the review page shows it.
struct Entry {
    claim: String,
    evidence: String,
    scope: String,
    validity: String,
    status: ProposalStatus,
    /// The proposing chat and run, when an agent proposed it.
    source: Option<(String, Option<String>)>,
}
impl Page {
    pub(crate) fn render(&self) -> String {
        let mut html = match &self.kind {
            Kind::Review(proposals) => {
                let mut h = heading(
                    "KNOWLEDGE",
                    "Review knowledge",
                    "Check the evidence before accepting a claim.",
                );
                if proposals.is_empty() {
                    h.push_str("<div class='empty'>No entries to review.</div>");
                }
                for (id, p) in proposals.iter().enumerate() {
                    h.push_str(&format!("<article class='record'><span class='tag'>{}</span><h3>{}</h3><dl><dt>Evidence</dt><dd>{}</dd><dt>Scope</dt><dd>{}</dd><dt>Recheck when</dt><dd>{}</dd></dl>", escape(p.status.as_str()), escape(&p.claim), escape(&p.evidence), escape(&p.scope), escape(&p.validity)));
                    if let Some((chat, run)) = &p.source {
                        h.push_str(&format!(
                            "<p class='muted'>From <a href='/dm/{}'>chat</a> · run {}</p>",
                            escape(chat),
                            escape(run.as_deref().unwrap_or("local operator"))
                        ));
                    }
                    if p.status == ProposalStatus::Pending {
                        h.push_str(&format!("<form method='post' action='/review' class='actions'><input type='hidden' name='id' value='{id}'><button class='primary' name='decision' value='accepted'>Accept entry</button><button name='decision' value='rejected'>Reject</button></form>"));
                    }
                    h.push_str("</article>");
                }
                h.push_str("<h3 class='form-title'>New entry</h3><form method='post' action='/propose'>");
                h.push_str(&input("claim", "Claim · up to 280 characters", 280));
                h.push_str(&input("evidence", "Evidence or source", 1000));
                h.push_str(&input("scope", "Scope", 300));
                h.push_str(&input("validity", "Recheck when", 300));
                h.push_str("<button class='primary'>Submit for review ↗</button></form>");
                h
            }
            Kind::Access(requests) => {
                let mut h = heading(
                    "ACCESS",
                    "Access requests",
                    "Preview only: decisions are recorded but do not grant access or run tools.",
                );
                if requests.is_empty() {
                    h.push_str("<div class='empty'>No pending requests.</div>");
                }
                for (id, r) in requests.iter().enumerate() {
                    h.push_str(&format!("<article class='record'><span class='tag'>{}</span><h3>{}</h3><dl><dt>Resource</dt><dd>{}</dd><dt>Purpose</dt><dd>{}</dd><dt>Route</dt><dd>{} {}</dd></dl>", escape(r.status.as_str()), escape(&r.capability), escape(&r.resource), escape(&r.purpose), escape(&r.authority), if r.authority == "judge" { "· manual review" } else { "· local operator" }));
                    if r.status == RequestStatus::Pending {
                        h.push_str(&format!("<form method='post' action='/decide' class='actions'><input type='hidden' name='id' value='{id}'><button class='primary' name='decision' value='approved'>Record approval</button><button name='decision' value='denied'>Deny</button></form>"));
                    }
                    h.push_str("</article>");
                }
                h.push_str("<h3 class='form-title'>New request</h3><form method='post' action='/request'><label>Capability<select name='capability'><option>network.read</option><option>environment.package</option><option>tool.access</option></select></label>");
                h.push_str(&input("resource", "Domain, package, or tool", 300));
                h.push_str(&input("purpose", "Why is it needed?", 500));
                h.push_str("<label>Reviewer<select name='authority'><option value='human'>Human operator</option><option value='judge'>Judge · not connected</option></select></label><button class='primary'>Submit request ↗</button></form>");
                h
            }
        };
        if !self.notice.is_empty() {
            html = format!(
                "<div class='callout' role='status'>{}</div>{html}",
                escape(&self.notice)
            );
        }
        html
    }
}

#[cfg(test)]
mod tests;
