use crate::escape;
use std::collections::HashMap;

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Collective {
    pub room: Vec<(String, String)>,
    proposals: Vec<Knowledge>,
    requests: Vec<Request>,
    pub notice: String,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Knowledge {
    claim: String,
    evidence: String,
    scope: String,
    validity: String,
    status: String,
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
    format!(
        "<span class='eyebrow'>{kicker}</span><h2>{title}</h2><p class='muted'>{description}</p>"
    )
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
                    && ["general", "engineering", "research"].contains(&channel.as_str())
                {
                    self.room.push((channel, text));
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
                    || scope.is_empty()
                    || validity.is_empty()
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
                {
                    if p.status == "pending"
                        && ["accepted", "rejected"].contains(&decision.as_str())
                    {
                        p.status = decision;
                    }
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
                {
                    if r.status == "pending" && ["approved", "denied"].contains(&decision.as_str())
                    {
                        r.status = decision;
                    }
                }
                "/?view=authority"
            }
            _ => "/",
        }
    }
    pub fn render(&self, view: &str, channel: &str) -> Option<String> {
        let mut html = match view {
            "rooms" => {
                let channel = if ["general", "engineering", "research"].contains(&channel) {
                    channel
                } else {
                    "general"
                };
                let mut h = heading("COLLABORATION / ROOMS", "Think out loud. Keep the signal.", "Room messages are conversation, not established knowledge. Messages are authored by the local operator; agents are not connected.");
                h.push_str("<div class='tabs'><a href='/?view=rooms&channel=general'># general</a><a href='/?view=rooms&channel=engineering'># engineering</a><a href='/?view=rooms&channel=research'># research</a></div>");
                h.push_str(&format!("<h3># {channel}</h3><div class='room-feed'>"));
                let mut count = 0;
                for (room, text) in &self.room {
                    if room == channel {
                        count += 1;
                        h.push_str(&format!("<article class='record'><span class='eyebrow'>OPERATOR / MESSAGE {count:03}</span><p class='preserve'>{}</p></article>", escape(text)));
                    }
                }
                if count == 0 {
                    h.push_str("<div class='empty'>No messages yet. Share a finding or ask for help.</div>");
                }
                h.push_str(&format!("</div><form method='post' action='/room'><input type='hidden' name='channel' value='{channel}'><label>Message<textarea name='message' required maxlength='2000' rows='4'></textarea></label><button class='primary'>Send to #{channel} ↗</button></form><div class='callout'>Found something durable? <a href='/?view=knowledge'>Propose a concise knowledge entry ↗</a></div>"));
                h
            }
            "knowledge" => {
                let mut h = heading("KNOWLEDGE / REVIEW INBOX", "Cement facts, not transcripts.", "Proposals require evidence, scope, and a recheck condition. Acceptance records an operator decision; it does not independently verify the claim.");
                if self.proposals.is_empty() {
                    h.push_str("<div class='empty'>No proposals. Room discussions do not enter the knowledge base automatically.</div>");
                }
                for (id, p) in self.proposals.iter().enumerate() {
                    h.push_str(&format!("<article class='record'><span class='tag'>{}</span><h3>{}</h3><dl><dt>Evidence</dt><dd>{}</dd><dt>Scope</dt><dd>{}</dd><dt>Recheck when</dt><dd>{}</dd></dl>", escape(&p.status), escape(&p.claim), escape(&p.evidence), escape(&p.scope), escape(&p.validity)));
                    if p.status == "pending" {
                        h.push_str(&format!("<form method='post' action='/review' class='actions'><input type='hidden' name='id' value='{id}'><button class='primary' name='decision' value='accepted'>Accept entry</button><button name='decision' value='rejected'>Reject</button></form>"));
                    }
                    h.push_str("</article>");
                }
                h.push_str("<h3 class='form-title'>Propose knowledge</h3><form method='post' action='/propose'>");
                h.push_str(&input(
                    "claim",
                    "One concise claim · 280 characters maximum",
                    280,
                ));
                h.push_str(&input(
                    "evidence",
                    "Evidence · artifact reference, source, or reproducible check",
                    1000,
                ));
                h.push_str(&input("scope", "Where does this apply?", 300));
                h.push_str(&input(
                    "validity",
                    "What would invalidate it or trigger a recheck?",
                    300,
                ));
                h.push_str("<button class='primary'>Submit for review ↗</button></form>");
                h
            }
            "authority" => {
                let mut h = heading("AUTHORITY / REQUEST QUEUE", "More capability. Explicit authority.", "Demonstration requests only. Decisions update records; they do not change host permissions, install packages, or contact a judge. All approvals are scoped to the current step.");
                if self.requests.is_empty() {
                    h.push_str(
                        "<div class='empty'>No capability requests waiting for judgment.</div>",
                    );
                }
                for (id, r) in self.requests.iter().enumerate() {
                    h.push_str(&format!("<article class='record'><span class='tag'>{}</span><h3>{}</h3><dl><dt>Resource</dt><dd>{}</dd><dt>Purpose</dt><dd>{}</dd><dt>Route</dt><dd>{} {}</dd><dt>Expiry</dt><dd>End of current step · conceptual</dd></dl>", escape(&r.status), escape(&r.capability), escape(&r.resource), escape(&r.purpose), escape(&r.authority), if r.authority == "judge" { "· judge not connected; manual review" } else { "· local operator" }));
                    if r.status == "pending" {
                        h.push_str(&format!("<form method='post' action='/decide' class='actions'><input type='hidden' name='id' value='{id}'><button class='primary' name='decision' value='approved'>Record approval</button><button name='decision' value='denied'>Deny</button></form>"));
                    }
                    h.push_str("</article>");
                }
                h.push_str("<h3 class='form-title'>Request a manifest amendment</h3><form method='post' action='/request'><label>Capability<select name='capability'><option>network.read</option><option>environment.package</option><option>tool.access</option></select></label>");
                h.push_str(&input(
                    "resource",
                    "Resource · domain, Nix package, or tool",
                    300,
                ));
                h.push_str(&input("purpose", "Why is it needed?", 500));
                h.push_str("<label>Authority route<select name='authority'><option value='human'>Human operator</option><option value='judge'>Judge · manual fallback in prototype</option></select></label><button class='primary'>Request capability ↗</button></form>");
                h
            }
            "personas" => {
                let mut h = heading("DECLARATIONS / PERSONA + RUN", "Describe intent. Bound execution.", "Read-only reference manifest. Nix evaluation, sandbox creation, workflow interpretation, and policy enforcement are not implemented.");
                h.push_str("<div class='callout'>A lens is a pure function: explicit context inputs → selected knowledge and tools. It has no node owner and grants no authority.</div><pre class='manifest'>");
                h.push_str(&escape(include_str!("../examples/engineer.nix")));
                h.push_str("</pre><div class='node-grid'><article><h3>Personality</h3><p>Concise, curious, evidence-seeking. Personality guides behavior.</p></article><article><h3>Policy</h3><p>Authority is checked separately from context selection. Capability changes require adjudication.</p></article><article><h3>Workflow</h3><p>The runtime should own loop conditions, iteration limits, and termination.</p></article></div>");
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
mod tests {
    use super::*;
    fn form(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
    #[test]
    fn knowledge_requires_evidence_and_rejects_duplicates() {
        let mut c = Collective::default();
        c.action("/propose", &form(&[("claim", "A fact")]));
        assert!(c.proposals.is_empty());
        let f = form(&[
            ("claim", "A fact"),
            ("evidence", "artifact:1"),
            ("scope", "test"),
            ("validity", "revision changes"),
        ]);
        c.action("/propose", &f);
        c.action("/propose", &f);
        assert_eq!(c.proposals.len(), 1);
        c.action("/review", &form(&[("id", "0"), ("decision", "accepted")]));
        c.action("/review", &form(&[("id", "0"), ("decision", "rejected")]));
        assert_eq!(c.proposals[0].status, "accepted");
    }
    #[test]
    fn requests_need_a_valid_route_and_decisions_are_terminal() {
        let mut c = Collective::default();
        let mut f = form(&[
            ("capability", "network.read"),
            ("resource", "example.org"),
            ("purpose", "Read docs"),
            ("authority", "auto"),
        ]);
        c.action("/request", &f);
        assert!(c.requests.is_empty());
        f.insert("authority".into(), "human".into());
        c.action("/request", &f);
        c.action("/decide", &form(&[("id", "0"), ("decision", "approved")]));
        c.action("/decide", &form(&[("id", "0"), ("decision", "denied")]));
        assert_eq!(c.requests[0].status, "approved");
    }
    #[test]
    fn rooms_escape_content_and_separate_channels() {
        let mut c = Collective::default();
        c.action(
            "/room",
            &form(&[
                ("channel", "research"),
                ("message", "<script>oops</script>"),
            ]),
        );
        assert!(!c.render("rooms", "general").unwrap().contains("oops"));
        let page = c.render("rooms", "research").unwrap();
        assert!(page.contains("&lt;script&gt;"));
        assert!(!page.contains("<script>"));
    }
}
