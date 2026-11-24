//! Manage → Permissions: the automation opt-ins in `permissions.json`
//! (`control::permissions`), the automatic approval policy, what is running
//! now with recent automatic decisions, and every request waiting on the
//! operator. The decisions themselves stay in each DM, where the exact
//! request is shown.
use control::permissions::{self, AutoApproval, KindMode, Mode};
use control_client::operator;
use std::io::Write;

const PATH: &str = "/manage/permissions";
const BACK: &str = "/?view=authority";

fn check(name: &str, label: &str, on: bool) -> String {
    format!(
        "<label><input type='checkbox' name='{name}' value='yes'{}> {label}</label>",
        if on { " checked" } else { "" }
    )
}
fn radio(name: &str, value: &str, label: &str, on: bool) -> String {
    format!(
        "<label><input type='radio' name='{name}' value='{value}'{}> {label}</label>",
        if on { " checked" } else { "" }
    )
}
fn kind_mode(name: &str, label: &str, mode: KindMode) -> String {
    format!(
        "<label>{label}<select name='{name}'><option value='auto'{}>Auto within the rules</option><option value='ask'{}>Always ask</option></select></label>",
        if mode == KindMode::Auto { " selected" } else { "" },
        if mode == KindMode::Ask { " selected" } else { "" }
    )
}

/// The node-wide switch, per-kind modes, standing targets, notices and strict principals.
fn auto_fieldset(policy: &AutoApproval) -> String {
    let mut principals = vec![(
        "local".to_owned(),
        "Local operator (this host, TUI, chat MCP)".to_owned(),
    )];
    if let Ok(access) = control_identity::Policy::load() {
        principals.extend(access.principals.iter().filter_map(|p| {
            p.user_id
                .map(|id| (format!("discord:{id}"), format!("Discord · {}", p.label)))
        }));
    }
    // Keep listed entries whose principal is no longer configured.
    for key in &policy.strict {
        if !principals.iter().any(|(listed, _)| listed == key) {
            principals.push((key.clone(), key.clone()));
        }
    }
    let strict = principals
        .iter()
        .map(|(key, label)| {
            check(
                &format!("strict_{key}"),
                &crate::escape(label),
                policy.strict.contains(key),
            )
        })
        .collect::<String>();
    format!(
        "<fieldset><legend>Automatic approval</legend>{}{}\
        <p class='muted'>Strict: every port and bot request waits for you. Auto: routine requests inside a task's scope are approved at once by the rules below, recorded with the policy as decider and shown under Auto-approved recently; anything else still asks.</p>\
        {}{}<p class='muted'>Folder access (host files) and anything involving credentials always ask; there is no automatic option.</p>\
        <label>Standing egress targets (host:port, one per line)<textarea name='standing_targets' rows='3' placeholder='127.0.0.1:25565'>{}</textarea></label>\
        <p class='muted'>Without a listed target, ports are automatic only for public host names on 80 or 443, routes between the task's own services and ingress above 1024 on host loopback. Loopback, private-network and link-local targets always ask unless listed here.</p>\
        {}<p class='muted'>Rate-limited FYI notices through your messaging channels; no action needed.</p>\
        <p class='muted'>Always ask for tasks of:</p>{strict}</fieldset>",
        radio(
            "auto_mode",
            "strict",
            "Strict (everything asks)",
            policy.mode == Mode::Strict
        ),
        radio(
            "auto_mode",
            "auto",
            "Auto (routine requests within scope)",
            policy.mode == Mode::Auto
        ),
        kind_mode("auto_port", "Network ports", policy.port),
        kind_mode("auto_bot", "Persistent bots", policy.bot),
        crate::escape(&policy.standing_targets.join("\n")),
        check(
            "auto_notices",
            "Send a notice for each automatic decision",
            policy.notices
        )
    )
}

/// The opt-ins form, what is running, recent automatic decisions and the node's pending requests.
pub(crate) fn render(state: &crate::State) -> String {
    format!("{}{}", form(), Page::of(state).render())
}
/// The opt-ins form. It reads `permissions.json` and the Discord access
/// policy, and no state, so the server renders it before the state lock.
pub(crate) fn form() -> String {
    let current = permissions::read();
    let kinds = [
        ("folder", "Folder access (/open: host paths)"),
        ("port", "Network ports"),
        ("bot", "Persistent bots"),
        ("question", "Design questions"),
        ("knowledge", "Knowledge proposals"),
    ]
    .iter()
    .map(|(kind, label)| check(&format!("mcp_{kind}"), label, current.mcp_approvals.allows(kind)))
    .collect::<String>();
    format!(
        "<h2>Permissions</h2><p class='muted'>Strict and off by default. Requests wait for a decision unless the automatic approval policy below takes them; the other settings only choose who else may decide.</p>\
        <form class='decision-form permissions' method='post' action='{PATH}'>{}\
        <fieldset><legend>The chat MCP (<code>control mcp --chat</code>) may decide</legend>{kinds}\
        <p class='muted'>An external agent with these on can approve its own run's requests. It already acts as you, so this is a policy for the bridge, not a security boundary.</p></fieldset>\
        <fieldset><legend>Control agents</legend>{}\
        <p class='muted'>Agents see and change only their own task, never decide requests and spend your model quota.</p></fieldset>\
        <button>Save</button></form>",
        auto_fieldset(&current.auto_approval),
        check(
            "agent_chat_tools",
            "Agents may use the chat tools (control_chat) to drive DMs in their own task",
            current.agent_chat_tools
        )
    )
}
/// The page below the form, copied out of state so it renders after the
/// state lock is released.
pub(crate) struct Page {
    running: crate::auto_approval::Overview,
    /// `control.requests.list`'s groups.
    waiting: serde_json::Value,
}
impl Page {
    pub(crate) fn of(state: &crate::State) -> Self {
        Self {
            running: crate::auto_approval::Overview::of(state),
            waiting: crate::chat::pending_requests(state),
        }
    }
    pub(crate) fn render(&self) -> String {
        let mut html = self.running.render();
        let mut items = String::new();
        for group in self.waiting.as_array().into_iter().flatten() {
            let chat = group["chat_id"].as_str().unwrap_or("");
            let title = crate::escape(group["title"].as_str().unwrap_or(chat));
            for pending in operator::pending(group) {
                let body = if pending.body.is_empty() {
                    String::new()
                } else {
                    format!("<p>{}</p>", crate::escape(pending.body))
                };
                items.push_str(&format!("<article class='record'><span class='tag'>{}</span><h3>{}</h3>{body}<p class='muted'><a href='/dm/{}'>{title} ↗</a></p></article>",
                    pending.kind.as_str(), crate::escape(&pending.heading), crate::escape(chat)));
            }
        }
        html.push_str("<h3 class='form-title'>Waiting on you</h3>");
        html.push_str(if items.is_empty() {
            "<div class='empty'>No pending requests.</div>"
        } else {
            &items
        });
        html
    }
}

/// `POST /manage/permissions` from the form above.
pub fn http(stream: &mut impl Write, method: &str, path: &str, body: &[u8]) -> bool {
    if path != PATH {
        return false;
    }
    let fields = crate::fields(&String::from_utf8_lossy(body));
    let result = if method == "POST" {
        permissions::save(&update(&fields))
    } else {
        Err("Use POST")
    };
    let _ = match result {
        Ok(()) => control_http::respond(
            stream,
            "303 See Other",
            "text/plain",
            b"",
            &format!("Location: {BACK}\r\n"),
            control_http::Policy::Page,
        ),
        Err(error) => control_http::respond(
            stream,
            "400 Bad Request",
            "text/html; charset=utf-8",
            crate::error_page(error, BACK).as_bytes(),
            "",
            control_http::Policy::Page,
        ),
    };
    true
}
/// The whole form is submitted, so an absent box is off and an absent
/// choice is the strict one.
fn update(fields: &std::collections::HashMap<String, String>) -> permissions::Permissions {
    let on = |name: &str| fields.get(name).is_some_and(|value| value == "yes");
    let kind = |name: &str| {
        if fields.get(name).is_some_and(|value| value == "auto") {
            KindMode::Auto
        } else {
            KindMode::Ask
        }
    };
    let auto_approval = if fields.contains_key("auto_mode") {
        AutoApproval {
            mode: if fields.get("auto_mode").is_some_and(|value| value == "auto") {
                Mode::Auto
            } else {
                Mode::Strict
            },
            port: kind("auto_port"),
            bot: kind("auto_bot"),
            standing_targets: fields
                .get("standing_targets")
                .map(|text| {
                    text.lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            strict: {
                let mut keys = fields
                    .iter()
                    .filter(|(_, value)| *value == "yes")
                    .filter_map(|(name, _)| name.strip_prefix("strict_").map(str::to_owned))
                    .collect::<Vec<_>>();
                keys.sort();
                keys
            },
            notices: on("auto_notices"),
        }
    } else {
        AutoApproval::default()
    };
    let mut permissions = permissions::Permissions {
        agent_chat_tools: on("agent_chat_tools"),
        auto_approval,
        ..Default::default()
    };
    for kind in permissions::Approvals::KINDS {
        permissions.mcp_approvals.set(kind, on(&format!("mcp_{kind}")));
    }
    permissions
}

#[cfg(test)]
mod tests;
