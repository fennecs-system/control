//! Wake-context size fixture shaped like the run -128 team: a lead with the
//! swarm and advisor lenses, a persistent member and the read-only advisor,
//! each woken by a room post after a dozen earlier callbacks.
use super::*;
use crate::delegation::{Persona, Role};

struct Team {
    state: State,
    lead: String,
    member: String,
    advisor: String,
}

fn chat_with(state: &mut State, parent: Option<&str>, lenses: &[&str]) -> String {
    let chat = crate::chat::create(state);
    let session = state.chats.iter_mut().find(|c| c.id == chat).unwrap();
    session.lenses = Some(lenses.iter().map(|s| (*s).to_owned()).collect());
    session.room = Some("general".into());
    session.parent_chat = parent.map(str::to_owned);
    chat
}

fn turn(state: &mut State, chat: &str, kind: crate::runtime::RunKind, text: &str, seed: Option<&crate::runtime::Run>, persona: Option<Persona>) -> String {
    state.runtime.enqueue_for(text.into(), kind, "test", crate::identity::Actor::LocalOperator, crate::identity::Action::Task).unwrap();
    let run = state.runtime.runs.last_mut().unwrap();
    run.chat_id = Some(chat.into());
    if let Some(seed) = seed {
        *run.context = (*seed.context).clone();
        run.lenses = seed.lenses.clone();
    }
    run.persona = persona;
    run.status = "completed".into();
    *run.result = "Claimed the next milestone; routine running; waiting on its events.".into();
    let id = run.id.clone();
    let session = state.chats.iter_mut().find(|c| c.id == chat).unwrap();
    session.messages.push(crate::chat::Message { speaker: Default::default(), text: text.into(), run_id: Some(id.clone()), reply: String::new() });
    id
}

fn team() -> Team {
    let mut state = State::default();
    let lead_chat = chat_with(&mut state, None, &["swarm", "advisor"]);
    crate::chat::send(&mut state, &lead_chat, "Build the shared deliverable with five members; the backlog is in #general.", "test").unwrap();
    let seed = state.runtime.runs[0].clone();
    assert!(seed.context.is_some(), "the lead captures its lens context");
    state.runtime.runs[0].status = "completed".into();
    let root = seed.id.clone();
    let member_chat = chat_with(&mut state, Some(&lead_chat), &["swarm", "advisor"]);
    let worker = Persona { role: Role::Worker, root: root.clone(), depth: 1, parent: Some(root.clone()) };
    turn(&mut state, &member_chat, crate::runtime::RunKind::Spawned, "Own member 2. Claim the next milestone from the shared backlog in #general and run it as a routine.", Some(&seed), Some(worker.clone()));
    let advisor_chat = chat_with(&mut state, Some(&lead_chat), &["swarm", "advisor"]);
    let judge = Persona { role: Role::Judge, root: root.clone(), depth: 1, parent: Some(root.clone()) };
    turn(&mut state, &advisor_chat, crate::runtime::RunKind::Spawned, "Advise the lead on the shared outcome.", Some(&seed), Some(judge.clone()));
    for chat in [&lead_chat, &member_chat] {
        let session = state.chats.iter_mut().find(|c| &c.id == chat).unwrap();
        session.continuity = Some(Box::new(Managed::default()));
    }
    for n in 0..12 {
        let event = format!("Control event: Message from member {n}: milestone {n} advanced; routine reached its target and notified; next milestone claimed. {}", "detail ".repeat(40));
        turn(&mut state, &lead_chat, crate::runtime::RunKind::AgentMessage, &event, Some(&seed), None);
        turn(&mut state, &member_chat, crate::runtime::RunKind::AgentMessage, &event, Some(&seed), Some(worker.clone()));
    }
    let lead = turn(&mut state, &lead_chat, crate::runtime::RunKind::BotWake, "Bot woke: new messages in room #general", Some(&seed), None);
    let member = turn(&mut state, &member_chat, crate::runtime::RunKind::BotWake, "Bot woke: new messages in room #general", Some(&seed), Some(worker));
    let advice = crate::bots::advice("members marked claims blocked", &lead_chat);
    let advisor = turn(&mut state, &advisor_chat, crate::runtime::RunKind::BotWake, &advice, Some(&seed), Some(judge));
    Team { state, lead, member, advisor }
}

/// Policy (lens and session guidance) and whole wake-context bytes per role.
fn sizes(team: &Team) -> [(&'static str, usize, usize); 3] {
    let managed = |id: &str| {
        let prepared = project(&team.state, id).unwrap();
        let policy = prepared.sections.iter().filter(|s| s.kind == SectionKind::Policy).map(|s| s.bytes).sum::<usize>();
        (policy, prepared.text.len())
    };
    let (lead_policy, lead_total) = managed(&team.lead);
    let (member_policy, member_total) = managed(&team.member);
    let run = team.state.runtime.runs.iter().find(|r| r.id == team.advisor).unwrap();
    let session = team.state.chats.iter().find(|c| Some(&c.id) == run.chat_id.as_ref()).unwrap();
    let advisor_policy = crate::chat::prompt_prefix(run, session).len();
    let advisor_total = crate::claude::render_prompt(&crate::chat::prompt(&team.state, &team.advisor).unwrap(), true).len();
    [("lead", lead_policy, lead_total), ("member", member_policy, member_total), ("advisor", advisor_policy, advisor_total)]
}

/// Tool schemas a session lists before (every callable tool) and after
/// deferred loading (role core + loaders), for a backend with or without
/// Control's workspace shell; and the prompt's deferred index.
fn tool_bytes(team: &Team, id: &str, shell: bool) -> (usize, usize, usize) {
    use crate::worker_tools::deferred;
    let run = team.state.runtime.runs.iter().find(|r| r.id == id).unwrap();
    let facts = deferred::Facts { shell, ..deferred::Facts::of(&team.state, run, crate::turn_context::managed(&team.state, run)) };
    let size = |tools: Vec<serde_json::Value>| serde_json::to_string(&tools).unwrap().len();
    (size(deferred::offered(&facts)), size(deferred::listed(&facts)), deferred::index(&facts).len())
}

#[test]
fn wake_policy_is_a_short_core_with_guidance_on_demand() {
    let team = team();
    let sizes = sizes(&team);
    let ids = [&team.lead, &team.member, &team.advisor];
    let report = sizes.iter().zip(ids).map(|((role, policy, total), id)| {
        let (before, after, index) = tool_bytes(&team, id, false);
        let (shell_before, shell_after, _) = tool_bytes(&team, id, true);
        serde_json::json!({"role":role,"policy_bytes":policy,"index_bytes":index,"wake_bytes":total,
            "tool_bytes_before":before,"tool_bytes_after":after,
            "tool_tokens_before":crate::local_agent::tokens(before),"tool_tokens_after":crate::local_agent::tokens(after),
            "shell_tool_bytes_before":shell_before,"shell_tool_bytes_after":shell_after,
            "wake_plus_tools_before":total - index + before,"wake_plus_tools_after":total + after})
    }).collect::<Vec<_>>();
    println!("{}", serde_json::json!({"roles":report}));
    if std::env::var_os("CONTROL_PRINT_POLICY").is_some() {
        println!("{}", project(&team.state, &team.member).unwrap().text);
    }
    // Before guidance on demand every role carried about 16-17 KB of policy;
    // the deferred tool index is bounded separately.
    let index = |id: &str| tool_bytes(&team, id, false).2;
    let [(_, lead, _), (_, member, _), (_, advisor, _)] = sizes;
    let (lead, member) = (lead - index(&team.lead), member - index(&team.member));
    assert!(member <= 3_072 && lead <= 3_328, "member {member}, lead {lead}");
    assert!(advisor <= 5_632, "advisor {advisor}: its own procedure is inline");
    for id in ids {
        assert!(index(id) <= 2_048, "index {}", index(id));
        for shell in [false, true] {
            let (before, after, _) = tool_bytes(&team, id, shell);
            // Every role's always-loaded schemas stay below 10 KB.
            assert!(after < 10_000 && after * 3 < before, "{after} of {before}");
        }
    }
    let text = |id: &str| project(&team.state, id).unwrap().text;
    let (lead, member) = (text(&team.lead), text(&team.member));
    let advisor = crate::chat::prompt(&team.state, &team.advisor).unwrap();
    let procedure = "Advisor: you are a read-only judge woken only by host facts";
    assert!(!member.contains("start one advisor") && !member.contains(procedure));
    assert!(lead.contains("- advisor: Lead: after spawning the team") && !lead.contains(procedure));
    assert!(advisor.contains(procedure));
    for text in [&lead, &member, &advisor] {
        assert!(text.contains("control_guidance") && !text.contains("Whoever steers a team posts the milestone backlog"));
        assert!(text.contains("More tools (names omit control_;"));
    }
    // Members find room_plan in the index; the lead has it loaded.
    assert!(member.contains("room_plan (") && !lead.contains("room_plan ("));
}

