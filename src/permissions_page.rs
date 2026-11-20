//! Manage → Permissions: the automation opt-ins in `permissions.json`
//! (`control::permissions`) and every request waiting on the operator. The
//! decisions themselves stay in each DM, where the exact request is shown.
use control::{client::operator, permissions};
use std::io::Write;

const PATH: &str = "/manage/permissions";
const BACK: &str = "/?view=authority";

fn check(name: &str, label: &str, on: bool) -> String {
    format!("<label><input type='checkbox' name='{name}' value='yes'{}> {label}</label>", if on { " checked" } else { "" })
}

/// The opt-ins form and the node's pending requests.
pub(crate) fn render(state: &crate::State) -> String {
    let current = permissions::read();
    let kinds = [("folder", "Folder access (/open: host paths)"), ("port", "Network ports"), ("bot", "Persistent bots"), ("question", "Design questions")]
        .iter().map(|(kind, label)| check(&format!("mcp_{kind}"), label, current.mcp_approvals.allows(kind))).collect::<String>();
    let mut html = format!("<h2>Permissions</h2><p class='muted'>Everything here is off by default. Requests always wait for a decision; these settings only choose who else may make it.</p>\
        <form class='decision-form permissions' method='post' action='{PATH}'>\
        <fieldset><legend>The chat MCP (<code>control mcp --chat</code>) may decide</legend>{kinds}\
        <p class='muted'>An external agent with these on can approve its own run's requests. It already acts as you, so this is a policy for the bridge, not a security boundary.</p></fieldset>\
        <fieldset><legend>Control agents</legend>{}\
        <p class='muted'>Agents see and change only their own task, never decide requests and spend your model quota.</p></fieldset>\
        <button>Save</button></form>",
        check("agent_chat_tools", "Agents may use the chat tools (control_chat) to drive DMs in their own task", current.agent_chat_tools));
    let groups = crate::chat::pending_requests(state);
    let mut items = String::new();
    for group in groups.as_array().into_iter().flatten() {
        let chat = group["chat_id"].as_str().unwrap_or("");
        let title = crate::escape(group["title"].as_str().unwrap_or(chat));
        for pending in operator::pending(group) {
            let body = if pending.body.is_empty() { String::new() } else { format!("<p>{}</p>", crate::escape(pending.body)) };
            items.push_str(&format!("<article class='record'><span class='tag'>{}</span><h3>{}</h3>{body}<p class='muted'><a href='/dm/{}'>{title} ↗</a></p></article>",
                pending.kind.as_str(), crate::escape(&pending.heading), crate::escape(chat)));
        }
    }
    html.push_str("<h3 class='form-title'>Waiting on you</h3>");
    html.push_str(if items.is_empty() { "<div class='empty'>No pending requests.</div>" } else { &items });
    html
}

/// `POST /manage/permissions` from the form above.
pub fn http(stream: &mut impl Write, method: &str, path: &str, body: &[u8]) -> bool {
    if path != PATH {
        return false;
    }
    let fields = crate::fields(&String::from_utf8_lossy(body));
    let result = if method == "POST" { permissions::save(&update(&fields)) } else { Err("Use POST") };
    let _ = match result {
        Ok(()) => control_http::respond(stream, "303 See Other", "text/plain", b"", &format!("Location: {BACK}\r\n"), control_http::Policy::Page),
        Err(error) => control_http::respond(stream, "400 Bad Request", "text/html; charset=utf-8", crate::error_page(error, BACK).as_bytes(), "", control_http::Policy::Page),
    };
    true
}
/// The whole form is submitted, so an absent box is off.
fn update(fields: &std::collections::HashMap<String, String>) -> permissions::Permissions {
    let on = |name: &str| fields.get(name).is_some_and(|value| value == "yes");
    let mut permissions = permissions::Permissions { agent_chat_tools: on("agent_chat_tools"), ..Default::default() };
    for kind in permissions::Approvals::KINDS {
        permissions.mcp_approvals.set(kind, on(&format!("mcp_{kind}")));
    }
    permissions
}

#[cfg(test)]
mod tests;
