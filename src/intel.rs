//! Intel: read-only summaries of teams and the node, and rate-limited notices
//! to the operator. Both are small generic tools built from records the host
//! already keeps (chats, runs, claims, score probes, usage, pending requests
//! and the advisor/antibody fact snapshots); nothing here acts on a team.
//!
//! - `control.intel.brief` (operator) and `control_intel_brief` (worker)
//!   render an **intel brief** in Markdown: per team its goal, progress (claims
//!   and score deltas), cost (tokens), observed pathologies and open operator
//!   requests, over the last 24 hours. A worker reads its own team; an approved
//!   antibody reads any team or the whole node. The operator call can save the
//!   brief under the state directory (`intel/brief-YYYY-MM-DD.md`).
//! - `control_operator_notify` (opt-in through the `operator.notify` lens tool,
//!   for an approved antibody or a lead) records a short notice that operator
//!   channels forward once, like a pending request that needs no decision.
//!   It is rate-limited node-wide (`operator_notices_per_hour`) and refuses a
//!   repeat of a shown notice.
//!
//! Channels deliver both through the generic `Transport`: see
//! `client::channel` and docs/setup.md (messaging channels).
use crate::{
    State,
    tool_registry::{AttemptCall, AttemptRef, AttemptTools, LocalHost},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, fmt::Write as _, io};

/// The window a brief summarises.
pub const WINDOW: u64 = 86_400;
/// Teams one brief lists, most tokens first.
const TEAMS: usize = 32;
/// Notices kept; older ones age out.
const NOTICES: usize = 32;
/// Notices show as pending in operator views this long.
pub const NOTICE_SECONDS: u64 = 86_400;
const NOTICE_CHARS: usize = 600;
pub(crate) const NOTIFY_TOOL: &str = "operator.notify";

/// One operator notice: no decision is requested; a reply steers the top-level chat of `chat`'s tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    /// The chat that sent it; replies steer this chat.
    pub chat: String,
    /// The team root it concerns, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub text: String,
    pub unix: u64,
}

fn now() -> u64 {
    crate::bots::now_ms() / 1000
}
fn fail(message: impl ToString) -> io::Error {
    io::Error::other(message.to_string())
}

/// `YYYY-MM-DD` (UTC) for a unix time.
pub fn date(unix: u64) -> String {
    control::client::channel::utc_date(unix)
}

/// Every chat's top-level chat.
struct Tree<'a> {
    parent: HashMap<&'a str, &'a str>,
}
impl<'a> Tree<'a> {
    fn new(state: &'a State) -> Self {
        Self { parent: state.chats.iter().filter_map(|chat| Some((chat.id.as_str(), chat.parent_chat.as_deref()?))).collect() }
    }
    fn root(&self, mut chat: &'a str) -> &'a str {
        for _ in 0..64 {
            match self.parent.get(chat) {
                Some(parent) => chat = parent,
                None => break,
            }
        }
        chat
    }
}

fn shorten(text: &str, chars: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}
/// Markdown-inert text: no headings, links or emphasis from untrusted input.
fn inert(text: &str) -> String {
    text.chars().map(|c| if matches!(c, '*' | '_' | '`' | '[' | ']' | '#' | '<' | '>' | '|') { ' ' } else { c }).collect()
}

/// One team's figures for the window.
#[derive(Default)]
struct Team<'a> {
    root: &'a str,
    members: usize,
    active: usize,
    turns: usize,
    tokens: u64,
    measured: usize,
    done: usize,
    open: usize,
    requests: (usize, usize, usize),
}

/// Renders the brief for `scope` (a team root or any chat in it; `None` for
/// the node) at `at`.
pub fn brief(state: &State, scope: Option<&str>, at: u64) -> Result<String, &'static str> {
    let tree = Tree::new(state);
    let since = at.saturating_sub(WINDOW);
    let wanted = match scope {
        Some(chat) => {
            state.chats.iter().find(|session| session.id == chat).ok_or("Chat not found.")?;
            Some(tree.root(chat))
        }
        None => None,
    };
    let mut teams: Vec<Team<'_>> = Vec::new();
    let mut slot: HashMap<&str, usize> = HashMap::new();
    for chat in &state.chats {
        let root = tree.root(&chat.id);
        if wanted.is_some_and(|wanted| wanted != root) {
            continue;
        }
        let index = *slot.entry(root).or_insert_with(|| {
            teams.push(Team { root, ..Default::default() });
            teams.len() - 1
        });
        let team = &mut teams[index];
        if chat.id != root {
            team.members += 1;
        }
        team.requests.0 += crate::design::pending(state, &chat.id).len();
        team.requests.1 += crate::bots::pending_count(state, &chat.id);
        team.requests.2 += crate::ports::pending_for_chat(state, &chat.id).len();
    }
    for run in &state.runtime.runs {
        let Some(&index) = run.chat_id.as_deref().and_then(|chat| slot.get(tree.root(chat))) else { continue };
        let team = &mut teams[index];
        if matches!(run.status.as_str(), "queued" | "running" | "waiting") {
            team.active += 1;
        }
        for attempt in run.attempts.iter().filter(|attempt| attempt.started_unix >= since) {
            team.turns += 1;
            if let Some(tokens) = attempt.tokens {
                team.tokens += tokens.input_tokens.saturating_add(tokens.output_tokens);
                team.measured += 1;
            }
        }
    }
    for claim in &state.collective.claims {
        let Some(&index) = slot.get(tree.root(&claim.owner)) else { continue };
        match claim.status {
            crate::rooms::claims::Status::Done => teams[index].done += 1,
            crate::rooms::claims::Status::Working | crate::rooms::claims::Status::Blocked if !claim.abandoned => teams[index].open += 1,
            _ => {}
        }
    }
    // A node brief lists teams active in the window: members, turns or a
    // pending request. Antibody trees are the observers, not a team.
    if wanted.is_none() {
        teams.retain(|team| (team.members > 0 || team.turns > 0) && (team.turns > 0 || team.active > 0 || team.requests != (0, 0, 0))
            && !crate::bots::antibody_offered(state, team.root));
    }
    teams.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(a.root.cmp(b.root)));
    let listed = teams.len();
    teams.truncate(TEAMS);

    let mut out = String::new();
    let heading = match wanted {
        Some(root) => format!("team {}", inert(&crate::callsigns::name_or_id(&state.chats, root))),
        None => "node".into(),
    };
    let _ = writeln!(out, "# Intel brief · {} · {heading}\n", date(at));
    let tokens = teams.iter().map(|team| team.tokens).sum::<u64>();
    let requests = teams.iter().map(|team| team.requests.0 + team.requests.1 + team.requests.2).sum::<usize>();
    let _ = writeln!(out, "Last 24 hours (UTC). Teams: {listed} · model turns: {} · tokens: {tokens} · open operator requests: {requests}\n",
        teams.iter().map(|team| team.turns).sum::<usize>());
    if teams.is_empty() {
        out.push_str("No team activity.\n");
    }
    for team in &teams {
        let session = state.chats.iter().find(|session| session.id == team.root);
        let title = inert(&crate::callsigns::name_or_id(&state.chats, team.root));
        let _ = writeln!(out, "## {title} ({})\n", team.root);
        if let Some(goal) = session.and_then(|session| session.messages.first()).map(|message| shorten(&inert(&message.text), 200)) {
            let _ = writeln!(out, "- Goal: {goal}");
        }
        let _ = writeln!(out, "- Members: {} · active runs: {} · model turns: {}", team.members, team.active, team.turns);
        let mut progress = format!("claims done {} · open {}", team.done, team.open);
        for probe in state.scores.iter().filter(|probe| tree.root(&probe.chat) == team.root).take(4) {
            let mut window = probe.samples.iter().filter(|sample| sample.unix >= since);
            let (Some(first), last) = (window.next(), probe.samples.back()) else { continue };
            let last = last.unwrap_or(first);
            for (key, value) in last.values.iter().take(4) {
                let start = first.values.get(key).copied().unwrap_or(*value);
                let _ = write!(progress, " · {}.{} {value} ({:+})", inert(&probe.name), inert(key), value - start);
            }
        }
        let _ = writeln!(out, "- Progress: {progress}");
        let _ = writeln!(out, "- Cost: {} tokens over {} measured attempt(s)", team.tokens, team.measured);
        match crate::bots::conditions(state, team.root) {
            Some(found) if found.is_empty() => out.push_str("- Pathologies: none observed\n"),
            Some(found) => { let _ = writeln!(out, "- Pathologies: {}", inert(&found.join("; "))); }
            None => out.push_str("- Pathologies: not observed (no advisor or antibody watches this team)\n"),
        }
        let (questions, bots, ports) = team.requests;
        if questions + bots + ports > 0 {
            let _ = writeln!(out, "- Waiting on the operator: {questions} question(s), {bots} bot request(s), {ports} port request(s)");
        }
        let notices = state.bots.notices.iter().filter(|notice| notice.unix >= since && notice.team.as_deref() == Some(team.root)).count();
        if notices > 0 {
            let _ = writeln!(out, "- Operator notices: {notices}");
        }
        out.push('\n');
    }
    if listed > teams.len() {
        let _ = writeln!(out, "{} more team(s) not listed.", listed - teams.len());
    }
    Ok(out)
}

/// Saves a brief as `intel/brief-DATE[-SCOPE].md` under the state directory.
pub fn save(markdown: &str, at: u64, scope: Option<&str>) -> Result<String, &'static str> {
    let name = match scope {
        Some(root) => format!("brief-{}-{root}.md", date(at)),
        None => format!("brief-{}.md", date(at)),
    };
    let path = crate::config::state(&format!("intel/{name}"))?;
    let directory = path.parent().ok_or("Invalid brief path.")?;
    std::fs::create_dir_all(directory).map_err(|_| "Could not create the intel directory.")?;
    crate::config::write_in(directory, &name, markdown.as_bytes()).map_err(|_| "Could not save the brief.")?;
    Ok(path.display().to_string())
}

/// `control.intel.brief {chat?, save?}` for operator clients.
pub fn tool(state: &State, arguments: &Value) -> Result<Value, &'static str> {
    let scope = match arguments.get("chat") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_str().filter(|id| crate::chat::valid_id(id)).ok_or("Invalid chat ID.")?),
    };
    let at = now();
    let markdown = brief(state, scope, at)?;
    let path = if arguments["save"] == true { Some(save(&markdown, at, scope.map(|chat| Tree::new(state).root(chat)))?) } else { None };
    Ok(json!({"date":date(at),"markdown":markdown,"path":path}))
}

/// Current notices, by the top-level chat their sender belongs to, for
/// operator request views. A notice shows for [`NOTICE_SECONDS`].
pub fn pending_json_tree(state: &State, root: &str) -> Vec<Value> {
    let cutoff = now().saturating_sub(NOTICE_SECONDS);
    let tree = Tree::new(state);
    state.bots.notices.iter().filter(|notice| notice.unix >= cutoff && tree.root(&notice.chat) == root)
        .map(|notice| json!({"notice_id":notice.id,"chat_id":notice.chat,"team":notice.team,"text":notice.text,"unix":notice.unix}))
        .collect()
}
pub fn pending_chats(state: &State) -> impl Iterator<Item = &str> {
    let cutoff = now().saturating_sub(NOTICE_SECONDS);
    state.bots.notices.iter().filter(move |notice| notice.unix >= cutoff).map(|notice| notice.chat.as_str())
}

/// Records a notice from `chat` after the opt-in, rate and duplicate checks.
pub(crate) fn notify(state: &mut State, chat: &str, team: Option<&str>, text: &str, id: &str, at: u64) -> Result<Notice, &'static str> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > NOTICE_CHARS {
        return Err("text must be 1–600 characters");
    }
    let tree = Tree::new(state);
    let own = tree.root(chat);
    if !crate::lens_library::offers(state, &crate::chat::selected_lenses(state.chats.iter().find(|session| session.id == own).ok_or("Chat not found")?), NOTIFY_TOOL) {
        return Err("Operator notices are opt-in: the root chat's lenses must offer operator.notify (the antibody lens does)");
    }
    let team = match team {
        Some(team) => {
            state.chats.iter().find(|session| session.id == team && session.parent_chat.is_none()).ok_or("team must be a top-level chat")?;
            Some(team.to_owned())
        }
        None => None,
    };
    if let Some(existing) = state.bots.notices.iter().find(|notice| notice.id == id) {
        return Ok(existing.clone());
    }
    let cutoff = at.saturating_sub(NOTICE_SECONDS);
    if state.bots.notices.iter().any(|notice| notice.unix >= cutoff && notice.chat == chat && notice.text == text) {
        return Err("This notice was already sent; notices are never repeated");
    }
    let limit = state.runtime.execution.operator_notices_per_hour;
    if state.bots.notices.iter().filter(|notice| notice.unix + 3600 > at).count() >= limit {
        return Err("The operator notice limit for this hour is reached; record the evidence and try again later only if it still matters");
    }
    let notice = Notice { id: id.to_owned(), chat: chat.to_owned(), team, text: text.to_owned(), unix: at };
    state.bots.notices.push(notice.clone());
    let excess = state.bots.notices.len().saturating_sub(NOTICES);
    state.bots.notices.drain(..excess);
    if let Some(session) = state.chats.iter_mut().find(|session| session.id == chat) {
        session.revision = session.revision.saturating_add(1);
    }
    Ok(notice)
}

pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![
            json!({"name":"control_intel_brief","description":"Read a Markdown intel brief for the last 24 hours: per team its goal, progress (claims and score probe changes), cost in tokens, pathologies observed by the host (stalls, repeated failures, repetition loops, polling, harness churn, cost burn) and requests waiting on the operator. Without chat_id it covers your own team; an approved antibody may name any chat to read that team, or omit chat_id to read the whole node. Read-only.","inputSchema":{"type":"object","properties":{"chat_id":{"type":"string"}},"additionalProperties":false},"annotations":{"readOnlyHint":true,"openWorldHint":false}}),
            json!({"name":"control_operator_notify","description":"Send the operator one short notice through their messaging channels (for example a Discord DM). No decision is requested; a reply goes to your task tree's steering chat. Use it only for what a team's lead is not fixing or what needs the operator: a team working against its goal, cost burn, a stuck team or a request waiting on the operator. team names the top-level chat it concerns. Opt-in (the root chat's lenses must offer operator.notify) and rate-limited per node; an identical notice is refused.","inputSchema":{"type":"object","properties":{"text":{"type":"string","minLength":1,"maxLength":NOTICE_CHARS},"team":{"type":"string"}},"required":["text"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":true}}),
        ]
    }
    fn known(&self, name: &str) -> bool {
        matches!(name, "control_intel_brief" | "control_operator_notify")
    }
    fn call(&self, cx: &AttemptCall<'_>, name: &str, args: &Value) -> io::Result<Value> {
        execute(cx.host, &cx.attempt, name, args)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BriefInput {
    chat_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotifyInput {
    text: String,
    team: Option<String>,
}

fn execute(host: &LocalHost<'_>, attempt: &AttemptRef<'_>, name: &str, args: &Value) -> io::Result<Value> {
    let mut state = host.0.lock().map_err(|_| fail("State unavailable"))?;
    let run = state.runtime.runs.iter()
        .find(|run| run.id == attempt.run && run.status == "running" && run.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(attempt.generation))
        .ok_or_else(|| fail("Attempt is not active"))?;
    crate::identity::authorize(&run.actor, run.action).map_err(fail)?;
    let judge = crate::delegation::is_judge(run);
    let chat = run.chat_id.clone().ok_or_else(|| fail("A chat is required"))?;
    let antibody = judge && state.bots.antibody(&chat);
    match name {
        "control_intel_brief" => {
            let input = serde_json::from_value::<BriefInput>(args.clone()).map_err(|_| fail("Expected an optional chat_id"))?;
            let tree = Tree::new(&state);
            let scope = match (input.chat_id.as_deref(), antibody) {
                (None, true) => None,
                (None, false) => Some(chat.as_str()),
                (Some(other), true) => Some(other),
                (Some(other), false) if tree.root(other) == tree.root(&chat) => Some(other),
                (Some(_), false) => return Err(fail("Read the brief of your own team; only an approved antibody reads other teams")),
            };
            let at = now();
            let markdown = brief(&state, scope, at).map_err(fail)?;
            Ok(json!({"date":date(at),"markdown":markdown}))
        }
        "control_operator_notify" => {
            let input = serde_json::from_value::<NotifyInput>(args.clone()).map_err(|_| fail("Expected text and an optional team"))?;
            // A lead (top-level chat) or an approved antibody may notify; a
            // member asks its lead, and other judges only review.
            let lead = state.chats.iter().any(|session| session.id == chat && session.parent_chat.is_none());
            if (judge && !antibody) || (!judge && !lead) {
                return Err(fail("Only an approved antibody or a team's lead may notify the operator"));
            }
            if !crate::chat::valid_id(attempt.nonce) {
                return Err(fail("Invalid request identity"));
            }
            let id = format!("notice-{}", crate::bots::digest_id(&[attempt.run, attempt.generation, attempt.nonce]));
            let previous = state.bots.notices.clone();
            let notice = notify(&mut state, &chat, input.team.as_deref(), &input.text, &id, now()).map_err(fail)?;
            if let Err(error) = crate::storage::save(&state) {
                state.bots.notices = previous;
                return Err(error);
            }
            Ok(json!({"notice_id":notice.id,"status":"recorded","note":"Recorded. The operator's channels forward it once; a reply goes to your task tree's steering chat."}))
        }
        _ => Err(fail("Unknown intel tool")),
    }
}

#[cfg(test)]
mod tests;
