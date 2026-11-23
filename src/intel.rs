//! Intel: read-only summaries of teams and the node, and rate-limited notices
//! to the operator. Both are small generic tools built from records the host
//! already keeps (chats, runs, claims, score probes, usage, pending requests
//! and the advisor/antibody fact snapshots); nothing here acts on a team.
//!
//! - `control.intel.brief` (operator) and `control_intel_brief` (worker)
//!   render an **intel brief** in Markdown: per team its goal, progress (claims
//!   and score deltas), cost (tokens), a bounded per-member activity table
//!   (turns, idle time, claims, routines, median turn and queue time, fresh
//!   tokens), observed pathologies and open operator requests, over the last
//!   24 hours. A worker reads its own team; an approved
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
use crate::runtime::RunStatus;
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

use crate::config::unix_seconds as now;

/// `YYYY-MM-DD` (UTC) for a unix time.
pub fn date(unix: u64) -> String {
    control_client::channel::utc_date(unix)
}

/// Every chat's top-level chat.
struct Tree<'a> {
    parent: HashMap<&'a str, &'a str>,
}
impl<'a> Tree<'a> {
    fn new(state: &'a State) -> Self {
        Self {
            parent: state
                .chats
                .iter()
                .filter_map(|chat| Some((chat.id.as_str(), chat.parent_chat.as_deref()?)))
                .collect(),
        }
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
    text.chars()
        .map(|c| {
            if matches!(c, '*' | '_' | '`' | '[' | ']' | '#' | '<' | '>' | '|') {
                ' '
            } else {
                c
            }
        })
        .collect()
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
    /// Its chats, as positions in `state.chats`.
    chats: Vec<usize>,
}

/// Rows of a team's member table.
const MEMBER_ROWS: usize = 12;
/// One member chat's activity: turns in the window, its newest turn of its
/// own, what runs now, its claims and routines, and per-turn timings.
#[derive(Default)]
struct Member<'a> {
    turns: usize,
    last: Option<u64>,
    now: Option<&'static str>,
    judge: bool,
    live: Vec<(&'a str, &'static str)>,
    done: usize,
    routines: usize,
    seconds: Vec<u64>,
    waits: Vec<u64>,
    fresh: u64,
}
fn median(values: &mut [u64]) -> Option<u64> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

/// The member table: the lead first, then the most recently active. A
/// retained peer message (a run with no attempt) is not a turn of its own.
fn member_table(
    state: &State,
    team: &Team<'_>,
    activity: &HashMap<&str, Member<'_>>,
    callsigns: &[crate::callsigns::Callsign],
    at: u64,
    out: &mut String,
) {
    let none = Member::default();
    let mut rows = team
        .chats
        .iter()
        .map(|&i| {
            (
                &state.chats[i],
                callsigns[i],
                activity.get(state.chats[i].id.as_str()).unwrap_or(&none),
            )
        })
        .collect::<Vec<_>>();
    // One-off helpers that are not working now would crowd the members out:
    // they are one count, not a row each.
    let standing = |chat: &crate::chat::Session, member: &Member<'_>| {
        chat.id == team.root
            || member.judge
            || member.now.is_some()
            || state.bots.advises(&chat.id)
            || state.bots.persists(&chat.id)
            || !member.live.is_empty()
            || member.routines > 0
    };
    let (helpers, spent) = rows
        .iter()
        .filter(|(chat, _, member)| !standing(chat, member))
        .fold((0usize, 0u64), |(count, fresh), (_, _, member)| {
            (count + 1, fresh + member.fresh)
        });
    rows.retain(|(chat, _, member)| standing(chat, member));
    rows.sort_by_key(|(chat, _, member)| (chat.id != team.root, std::cmp::Reverse(member.last)));
    out.push_str(
        "- Per member (24 h turns · since last turn · now · claims · routines · median turn/queue · fresh tokens):\n",
    );
    for (chat, callsign, member) in rows.iter().take(MEMBER_ROWS) {
        let role = if chat.id == team.root {
            "lead"
        } else if member.judge || state.bots.advises(&chat.id) {
            "advisor"
        } else if state.bots.persists(&chat.id) {
            "member"
        } else {
            "helper"
        };
        let last = member.last.map_or("no turn".into(), |last| {
            format!("{}m ago", at.saturating_sub(last) / 60)
        });
        let mut claims = member
            .live
            .iter()
            .take(2)
            .map(|(key, status)| format!("{} {status}", shorten(&inert(key), 24)))
            .collect::<Vec<_>>();
        if member.live.len() > 2 {
            claims.push(format!("+{}", member.live.len() - 2));
        }
        if claims.is_empty() {
            claims.push("no claim".into());
        }
        if member.done > 0 {
            claims.push(format!("{} done", member.done));
        }
        let _ = write!(
            out,
            "  - {} ({role}): {} turns · {last} · {} · {}",
            shorten(&inert(&crate::callsigns::label(chat, *callsign)), 24),
            member.turns,
            member.now.unwrap_or("idle"),
            claims.join(", ")
        );
        if member.routines > 0 {
            let _ = write!(out, " · {} routine(s)", member.routines);
        }
        let (mut seconds_taken, mut waits) = (member.seconds.clone(), member.waits.clone());
        let seconds = |value: Option<u64>| value.map_or("-".into(), |value| format!("{value}s"));
        let (turn, wait) = (median(&mut seconds_taken), median(&mut waits));
        if turn.is_some() || wait.is_some() {
            let _ = write!(out, " · {}/{}", seconds(turn), seconds(wait));
        }
        let _ = writeln!(out, " · {} tok", crate::history_view::compact(member.fresh));
    }
    if rows.len() > MEMBER_ROWS {
        let _ = writeln!(out, "  - {} more member(s) not listed", rows.len() - MEMBER_ROWS);
    }
    if helpers > 0 {
        let _ = writeln!(
            out,
            "  - {helpers} finished helper(s): {} tok",
            crate::history_view::compact(spent)
        );
    }
}

/// Renders the brief for `scope` (a team root or any chat in it; `None` for
/// the node) at `at`.
pub fn brief(state: &State, scope: Option<&str>, at: u64) -> Result<String, &'static str> {
    let tree = Tree::new(state);
    let since = at.saturating_sub(WINDOW);
    let wanted = match scope {
        Some(chat) => {
            state
                .chats
                .iter()
                .find(|session| session.id == chat)
                .ok_or("Chat not found.")?;
            Some(tree.root(chat))
        }
        None => None,
    };
    let mut teams: Vec<Team<'_>> = Vec::new();
    let mut slot: HashMap<&str, usize> = HashMap::new();
    for (position, chat) in state.chats.iter().enumerate() {
        let root = tree.root(&chat.id);
        if wanted.is_some_and(|wanted| wanted != root) {
            continue;
        }
        let index = *slot.entry(root).or_insert_with(|| {
            teams.push(Team {
                root,
                ..Default::default()
            });
            teams.len() - 1
        });
        let team = &mut teams[index];
        team.chats.push(position);
        if chat.id != root {
            team.members += 1;
        }
        team.requests.0 += crate::design::pending(state, &chat.id).len();
        team.requests.1 += crate::bots::pending_count(state, &chat.id);
        team.requests.2 += crate::ports::pending_for_chat(state, &chat.id).len();
    }
    let mut activity: HashMap<&str, Member<'_>> = HashMap::new();
    for run in state.runtime.runs.iter() {
        let Some(chat) = run.chat_id.as_deref() else { continue };
        let Some(&index) = slot.get(tree.root(chat)) else {
            continue;
        };
        let team = &mut teams[index];
        let member = activity.entry(chat).or_default();
        member.judge |= crate::delegation::is_judge(run);
        match run.status {
            RunStatus::Running | RunStatus::Waiting | RunStatus::Cancelling => member.now = Some("running"),
            RunStatus::Queued => {
                member.now.get_or_insert("queued");
            }
            RunStatus::Completed
            | RunStatus::Blocked
            | RunStatus::Failed
            | RunStatus::Interrupted
            | RunStatus::Cancelled
            | RunStatus::Continued
            | RunStatus::Unrecognised(_) => {}
        }
        if run.status.is_proceeding() {
            team.active += 1;
        }
        member.last = member
            .last
            .max(run.attempts.iter().map(|attempt| attempt.started_unix).max());
        // Queue wait: the run's creation (its ID) to its first attempt.
        if let (Some(first), Some(created)) = (
            run.attempts.first().filter(|attempt| attempt.started_unix >= since),
            run.created_ms(),
        ) {
            member
                .waits
                .push((first.started_unix * 1000).saturating_sub(created) / 1000);
        }
        for attempt in run.attempts.iter().filter(|attempt| attempt.started_unix >= since) {
            team.turns += 1;
            member.turns += 1;
            if attempt.elapsed_ms > 0 {
                member.seconds.push(attempt.elapsed_ms / 1000);
            }
            if let Some(tokens) = attempt.tokens {
                team.tokens += tokens.input_tokens.saturating_add(tokens.output_tokens);
                team.measured += 1;
                // Fresh tokens, as cost burn counts them: uncached input and output.
                member.fresh += tokens
                    .input_tokens
                    .saturating_sub(tokens.cached_input_tokens)
                    .saturating_add(tokens.output_tokens);
            }
        }
    }
    for watch in state.watches.iter().filter(|watch| watch.active()) {
        activity.entry(watch.chat.as_str()).or_default().routines += 1;
    }
    for claim in &state.collective.claims {
        let Some(&index) = slot.get(tree.root(&claim.owner)) else {
            continue;
        };
        let member = activity.entry(&claim.owner).or_default();
        match claim.status {
            crate::rooms::claims::Status::Done => member.done += 1,
            crate::rooms::claims::Status::Working if !claim.abandoned => member.live.push((&claim.key, "working")),
            crate::rooms::claims::Status::Blocked if !claim.abandoned => member.live.push((&claim.key, "blocked")),
            _ => {}
        }
        match claim.status {
            crate::rooms::claims::Status::Done => teams[index].done += 1,
            crate::rooms::claims::Status::Working | crate::rooms::claims::Status::Blocked if !claim.abandoned => {
                teams[index].open += 1
            }
            _ => {}
        }
    }
    // A node brief lists teams active in the window: members, turns or a
    // pending request. Antibody trees are the observers, not a team.
    if wanted.is_none() {
        teams.retain(|team| {
            (team.members > 0 || team.turns > 0)
                && (team.turns > 0 || team.active > 0 || team.requests != (0, 0, 0))
                && !crate::bots::antibody_offered(state, team.root)
        });
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
    let requests = teams
        .iter()
        .map(|team| team.requests.0 + team.requests.1 + team.requests.2)
        .sum::<usize>();
    let _ = writeln!(
        out,
        "Last 24 hours (UTC). Teams: {listed} · model turns: {} · tokens: {tokens} · open operator requests: {requests}\n",
        teams.iter().map(|team| team.turns).sum::<usize>()
    );
    if teams.is_empty() {
        out.push_str("No team activity.\n");
    }
    let callsigns = if teams.is_empty() {
        Vec::new()
    } else {
        crate::callsigns::all(&state.chats)
    };
    for team in &teams {
        let session = state.chats.iter().find(|session| session.id == team.root);
        let title = inert(&crate::callsigns::name_or_id(&state.chats, team.root));
        let _ = writeln!(out, "## {title} ({})\n", team.root);
        if let Some(goal) = session
            .and_then(|session| {
                session
                    .messages
                    .iter()
                    .find(|message| !message.text.trim_start().starts_with('/'))
            })
            .map(|message| shorten(&inert(&message.text), 200))
        {
            let _ = writeln!(out, "- Goal: {goal}");
        }
        let _ = writeln!(
            out,
            "- Members: {} · active runs: {} · model turns: {}",
            team.members, team.active, team.turns
        );
        let mut progress = format!("claims done {} · open {}", team.done, team.open);
        for probe in state
            .scores
            .iter()
            .filter(|probe| tree.root(&probe.chat) == team.root)
            .take(4)
        {
            let mut window = probe.samples.iter().filter(|sample| sample.unix >= since);
            let (Some(first), last) = (window.next(), probe.samples.back()) else {
                continue;
            };
            let last = last.unwrap_or(first);
            for (key, value) in last.values.iter().take(4) {
                let start = first.values.get(key).copied().unwrap_or(*value);
                let _ = write!(
                    progress,
                    " · {}.{} {value} ({:+})",
                    inert(&probe.name),
                    inert(key),
                    value - start
                );
            }
        }
        let _ = writeln!(out, "- Progress: {progress}");
        let _ = writeln!(
            out,
            "- Cost: {} tokens over {} measured attempt(s)",
            team.tokens, team.measured
        );
        // The node-wide brief lists every team of the day: only one asked for
        // by name, or one working now, gets its per-member table.
        if scope.is_some() || team.active > 0 {
            member_table(state, team, &activity, &callsigns, at, &mut out);
        }
        match crate::bots::conditions(state, team.root) {
            Some(found) if found.is_empty() => out.push_str("- Pathologies: none observed\n"),
            Some(found) => {
                let _ = writeln!(out, "- Pathologies: {}", inert(&found.join("; ")));
            }
            None => out.push_str("- Pathologies: not observed (no advisor or antibody watches this team)\n"),
        }
        let (questions, bots, ports) = team.requests;
        if questions + bots + ports > 0 {
            let _ = writeln!(
                out,
                "- Waiting on the operator: {questions} question(s), {bots} bot request(s), {ports} port request(s)"
            );
        }
        let notices = state
            .bots
            .notices
            .iter()
            .filter(|notice| notice.unix >= since && notice.team.as_deref() == Some(team.root))
            .count();
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
        Some(value) => Some(
            value
                .as_str()
                .filter(|id| crate::chat::valid_id(id))
                .ok_or("Invalid chat ID.")?,
        ),
    };
    let at = now();
    let markdown = brief(state, scope, at)?;
    let path = if arguments["save"] == true {
        Some(save(&markdown, at, scope.map(|chat| Tree::new(state).root(chat)))?)
    } else {
        None
    };
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
    state
        .bots
        .notices
        .iter()
        .filter(move |notice| notice.unix >= cutoff)
        .map(|notice| notice.chat.as_str())
}

/// Records a notice from `chat` after the opt-in, rate and duplicate checks.
pub(crate) fn notify(
    state: &mut State,
    chat: &str,
    team: Option<&str>,
    text: &str,
    id: &str,
    at: u64,
) -> Result<Notice, &'static str> {
    let text = text.trim();
    if text.is_empty() || text.chars().count() > NOTICE_CHARS {
        return Err("text must be 1–600 characters");
    }
    let tree = Tree::new(state);
    let own = tree.root(chat);
    if !crate::lens_library::offers(
        &state.lens_library,
        &crate::chat::selected_lenses(
            state
                .chats
                .iter()
                .find(|session| session.id == own)
                .ok_or("Chat not found")?,
        ),
        NOTIFY_TOOL,
    ) {
        return Err(
            "Operator notices are opt-in: the root chat's lenses must offer operator.notify (the antibody lens does)",
        );
    }
    let team = match team {
        Some(team) => {
            state
                .chats
                .iter()
                .find(|session| session.id == team && session.parent_chat.is_none())
                .ok_or("team must be a top-level chat")?;
            Some(team.to_owned())
        }
        None => None,
    };
    if let Some(existing) = state.bots.notices.iter().find(|notice| notice.id == id) {
        return Ok(existing.clone());
    }
    let cutoff = at.saturating_sub(NOTICE_SECONDS);
    if state
        .bots
        .notices
        .iter()
        .any(|notice| notice.unix >= cutoff && notice.chat == chat && notice.text == text)
    {
        return Err("This notice was already sent; notices are never repeated");
    }
    let limit = state.runtime.execution.operator_notices_per_hour;
    if state
        .bots
        .notices
        .iter()
        .filter(|notice| !notice.id.starts_with(POLICY_PREFIX) && notice.unix + 3600 > at)
        .count()
        >= limit
    {
        return Err(
            "The operator notice limit for this hour is reached; record the evidence and try again later only if it still matters",
        );
    }
    let notice = Notice {
        id: id.to_owned(),
        chat: chat.to_owned(),
        team,
        text: text.to_owned(),
        unix: at,
    };
    state.bots.notices.push(notice.clone());
    let excess = state.bots.notices.len().saturating_sub(NOTICES);
    state.bots.notices.drain(..excess);
    if let Some(session) = state.chats.iter_mut().find(|session| session.id == chat) {
        session.revision = session.revision.saturating_add(1);
    }
    Ok(notice)
}

/// FYI notices of automatic approvals forwarded per hour; beyond it the
/// decisions are still recorded and shown, just not forwarded. They have their
/// own budget, so they never use up agents' `operator_notices_per_hour`.
pub(crate) const POLICY_NOTICES_PER_HOUR: usize = 6;
const POLICY_PREFIX: &str = "policy-";

/// Records the FYI notice of an automatic approval (`id` starts with
/// `policy-`) from `chat`, unless this hour's policy budget is spent.
pub(crate) fn policy_notice(state: &mut State, chat: &str, text: &str, id: &str, at: u64) -> bool {
    debug_assert!(id.starts_with(POLICY_PREFIX));
    let sent = state
        .bots
        .notices
        .iter()
        .filter(|notice| notice.id.starts_with(POLICY_PREFIX) && notice.unix + 3600 > at)
        .count();
    if sent >= POLICY_NOTICES_PER_HOUR || state.bots.notices.iter().any(|notice| notice.id == id) {
        return false;
    }
    let team = Some(Tree::new(state).root(chat).to_owned());
    state.bots.notices.push(Notice {
        id: id.to_owned(),
        chat: chat.to_owned(),
        team,
        text: shorten(text, NOTICE_CHARS),
        unix: at,
    });
    let excess = state.bots.notices.len().saturating_sub(NOTICES);
    state.bots.notices.drain(..excess);
    true
}

pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![
            json!({
                "name": "control_intel_brief",
                "description": concat!(
                    "Read a Markdown intel brief for the last 24 hours: per team its goal, progress (claims and score ",
                    "probe changes), cost in tokens, pathologies observed by the host (stalls, repeated failures, ",
                    "repetition loops, polling, harness churn, cost burn) and requests waiting on the operator. ",
                    "Without chat_id it covers your own team; an approved antibody may name any chat to read that ",
                    "team, or omit chat_id to read the whole node. Read-only."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {"chat_id": {"type": "string"}},
                    "additionalProperties": false
                },
                "annotations": {"readOnlyHint": true, "openWorldHint": false}
            }),
            json!({
                "name": "control_operator_notify",
                "description": concat!(
                    "Send the operator one short notice through their messaging channels (for example a Discord DM). ",
                    "No decision is requested; a reply goes to your task tree's steering chat. Use it only for what a ",
                    "team's lead is not fixing or what needs the operator: a team working against its goal, cost ",
                    "burn, a stuck team or a request waiting on the operator. team names the top-level chat it ",
                    "concerns. Opt-in (the root chat's lenses must offer operator.notify) and rate-limited per node; ",
                    "an identical notice is refused."
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string", "minLength": 1, "maxLength": NOTICE_CHARS},
                        "team": {"type": "string"}
                    },
                    "required": ["text"],
                    "additionalProperties": false
                },
                "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": true}
            }),
        ]
    }
    fn names(&self) -> &'static [&'static str] {
        &["control_intel_brief", "control_operator_notify"]
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
    let mut state = host.0.lock().map_err(|_| io::Error::other("State unavailable"))?;
    let run = state
        .runtime
        .runs
        .get(attempt.run)
        .filter(|run| {
            run.status == RunStatus::Running
                && run.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(attempt.generation)
        })
        .ok_or_else(|| io::Error::other("Attempt is not active"))?;
    crate::identity::authorize(&run.actor, run.action).map_err(io::Error::other)?;
    // Node visibility and operator escalation are the run's standing.
    let standing = crate::personas::of(&state.bots, run);
    let judge = standing.read_only();
    let chat = run
        .chat_id
        .clone()
        .ok_or_else(|| io::Error::other("A chat is required"))?;
    let antibody = standing.antibody() && state.bots.antibody(&chat);
    match name {
        "control_intel_brief" => {
            let input = serde_json::from_value::<BriefInput>(args.clone())
                .map_err(|_| io::Error::other("Expected an optional chat_id"))?;
            let tree = Tree::new(&state);
            let scope = match (input.chat_id.as_deref(), antibody) {
                (None, true) => None,
                (None, false) => Some(chat.as_str()),
                (Some(other), true) => Some(other),
                (Some(other), false) if tree.root(other) == tree.root(&chat) => Some(other),
                (Some(_), false) => {
                    return Err(io::Error::other(
                        "Read the brief of your own team; only an approved antibody reads other teams",
                    ));
                }
            };
            let at = now();
            let markdown = brief(&state, scope, at).map_err(io::Error::other)?;
            Ok(json!({"date":date(at),"markdown":markdown}))
        }
        "control_operator_notify" => {
            let input = serde_json::from_value::<NotifyInput>(args.clone())
                .map_err(|_| io::Error::other("Expected text and an optional team"))?;
            // A lead (top-level chat) or an approved antibody may notify; a
            // member asks its lead, and other judges only review.
            let lead = state
                .chats
                .iter()
                .any(|session| session.id == chat && session.parent_chat.is_none());
            if !standing.notifies() || (judge && !antibody) || (!judge && !lead) {
                return Err(io::Error::other(
                    "Only an approved antibody or a team's lead may notify the operator",
                ));
            }
            if !crate::chat::valid_id(attempt.nonce) {
                return Err(io::Error::other("Invalid request identity"));
            }
            let id = format!(
                "notice-{}",
                crate::bots::digest_id(&[attempt.run, attempt.generation, attempt.nonce])
            );
            let previous = state.bots.notices.clone();
            let notice =
                notify(&mut state, &chat, input.team.as_deref(), &input.text, &id, now()).map_err(io::Error::other)?;
            if let Err(error) = crate::storage::save(&state) {
                state.bots.notices = previous;
                return Err(error);
            }
            Ok(
                json!({"notice_id":notice.id,"status":"recorded","note":"Recorded. The operator's channels forward it once; a reply goes to your task tree's steering chat."}),
            )
        }
        _ => Err(io::Error::other("Unknown intel tool")),
    }
}

#[cfg(test)]
mod tests;
