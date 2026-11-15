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
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Knowledge {
    pub claim: String,
    pub evidence: String,
    pub scope: String,
    pub validity: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::knowledge_tools::Source>,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Request {
    capability: String,
    resource: String,
    purpose: String,
    authority: String,
    status: String,
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
                if !text.is_empty()
                    && text.chars().count() <= 2000
                    && crate::rooms::valid(&channel)
                {
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
                    self.notice = "A proposal needs a claim of at most 280 characters, evidence, scope, and a recheck condition.".into();
                } else if self.proposals.iter().any(|p| {
                    p.status != "rejected" && p.claim.to_lowercase() == claim.to_lowercase()
                }) {
                    self.notice =
                        "That claim already exists. Review the existing record instead.".into();
                } else {
                    self.proposals.push(Knowledge {
                        claim,
                        evidence,
                        scope,
                        validity,
                        status: "pending".into(),
                        source: None,
                    });
                }
                "/?view=knowledge"
            }
            "/review" => {
                let decision = value(f, "decision");
                if let Some(p) = value(f, "id")
                    .parse::<usize>()
                    .ok()
                    .and_then(|id| self.proposals.get_mut(id))
                    && p.status == "pending"
                        && ["accepted", "rejected"].contains(&decision.as_str())
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
                if ["network.read", "environment.package", "tool.access"]
                    .contains(&capability.as_str())
                    && ["human", "judge"].contains(&authority.as_str())
                    && !resource.is_empty()
                    && !purpose.is_empty()
                {
                    self.requests.push(Request {
                        capability,
                        resource,
                        purpose,
                        authority,
                        status: "pending".into(),
                    });
                }
                "/?view=authority"
            }
            "/decide" => {
                let decision = value(f, "decision");
                if let Some(r) = value(f, "id")
                    .parse::<usize>()
                    .ok()
                    .and_then(|id| self.requests.get_mut(id))
                    && r.status == "pending" && ["approved", "denied"].contains(&decision.as_str())
                    {
                        r.status = decision;
                    }
                "/?view=authority"
            }
            _ => "/",
        }
    }
    pub fn render(&self, view: &str, _channel: &str) -> Option<String> {
        let mut html = match view {
            "knowledge-review" => {
                let mut h = heading(
                    "KNOWLEDGE",
                    "Review knowledge",
                    "Check the evidence before accepting a claim.",
                );
                if self.proposals.is_empty() {
                    h.push_str("<div class='empty'>No entries to review.</div>");
                }
                for (id, p) in self.proposals.iter().enumerate() {
                    h.push_str(&format!("<article class='record'><span class='tag'>{}</span><h3>{}</h3><dl><dt>Evidence</dt><dd>{}</dd><dt>Scope</dt><dd>{}</dd><dt>Recheck when</dt><dd>{}</dd></dl>", escape(&p.status), escape(&p.claim), escape(&p.evidence), escape(&p.scope), escape(&p.validity)));
                    if let Some(source) = &p.source {
                        h.push_str(&format!("<p class='muted'>From <a href='/chat/{}'>chat</a> · run {}</p>",escape(&source.chat),escape(source.run.as_deref().unwrap_or("local operator"))));
                    }
                    if p.status == "pending" {
                        h.push_str(&format!("<form method='post' action='/review' class='actions'><input type='hidden' name='id' value='{id}'><button class='primary' name='decision' value='accepted'>Accept entry</button><button name='decision' value='rejected'>Reject</button></form>"));
                    }
                    h.push_str("</article>");
                }
                h.push_str(
                    "<h3 class='form-title'>New entry</h3><form method='post' action='/propose'>",
                );
                h.push_str(&input("claim", "Claim · up to 280 characters", 280));
                h.push_str(&input("evidence", "Evidence or source", 1000));
                h.push_str(&input("scope", "Scope", 300));
                h.push_str(&input("validity", "Recheck when", 300));
                h.push_str("<button class='primary'>Submit for review ↗</button></form>");
                h
            }
            "authority" => {
                let mut h = heading(
                    "ACCESS",
                    "Access requests",
                    "Preview only: decisions are recorded but do not grant access or run tools.",
                );
                if self.requests.is_empty() {
                    h.push_str("<div class='empty'>No pending requests.</div>");
                }
                for (id, r) in self.requests.iter().enumerate() {
                    h.push_str(&format!("<article class='record'><span class='tag'>{}</span><h3>{}</h3><dl><dt>Resource</dt><dd>{}</dd><dt>Purpose</dt><dd>{}</dd><dt>Route</dt><dd>{} {}</dd></dl>", escape(&r.status), escape(&r.capability), escape(&r.resource), escape(&r.purpose), escape(&r.authority), if r.authority == "judge" { "· manual review" } else { "· local operator" }));
                    if r.status == "pending" {
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
            "personas" => {
                let mut h = heading(
                    "PERSONAS",
                    "Engineer",
                    "Reference only. This declaration does not configure agent runs.",
                );
                h.push_str("<details class='record'><summary>View declaration</summary><pre class='manifest'>");
                h.push_str(&escape(include_str!("../examples/engineer.nix")));
                h.push_str("</pre></details>");
                h
            }
            _ => return None,
        };
        if !self.notice.is_empty() {
            html = format!(
                "<div class='callout' role='status'>{}</div>{html}",
                escape(&self.notice)
            );
        }
        Some(html)
    }
}

#[cfg(test)]
mod tests;
