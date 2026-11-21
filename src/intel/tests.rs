use super::*;

/// A team root with one worker; the worker's turn spent tokens just now.
fn team(state: &mut State, goal: &str) -> (String, String) {
    let root = crate::chat::create(state);
    crate::chat::send(state, &root, goal, "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    let worker = crate::chat::create(state);
    state.chats.iter_mut().find(|chat| chat.id == worker).unwrap().parent_chat = Some(root.clone());
    crate::chat::send(state, &worker, "Build the wall", "test").unwrap();
    let run = state.runtime.runs.last_mut().unwrap();
    run.status = "completed".into();
    run.attempts = vec![crate::usage::Attempt { started_unix: now(), tokens: Some(control_ai::Tokens { input_tokens: 1_200, output_tokens: 300, ..Default::default() }), ..Default::default() }];
    (root, worker)
}

#[test]
fn dates_are_utc_calendar_days() {
    assert_eq!(date(0), "1970-01-01");
    assert_eq!(date(951_782_400), "2000-02-29");
    assert_eq!(date(1_795_000_000), "2026-11-18");
}

#[test]
fn a_node_brief_summarises_each_active_team_without_trusting_its_text() {
    let mut state = State::default();
    let (root, worker) = team(&mut state, "Build a **stone** wall [here](http://x) # now");
    let (quiet, _) = team(&mut state, "Old work");
    state.runtime.runs.iter_mut().filter(|run| run.chat_id.as_deref().is_some_and(|chat| chat != root && chat != worker)).for_each(|run| run.attempts.clear());
    state.scores.push(serde_json::from_value(json!({"chat":root,"name":"wall","service":"svc","generation":1,"command":"x","interval_seconds":300,"declared_unix":1,
        "samples":[{"unix":now() - 60,"trigger":"schedule","values":{"blocks":10.0}},{"unix":now(),"trigger":"schedule","values":{"blocks":25.0}}]})).unwrap());
    let markdown = brief(&state, None, now()).unwrap();
    assert!(markdown.starts_with(&format!("# Intel brief · {} · node", date(now()))), "{markdown}");
    assert!(markdown.contains("Teams: 1 ·") && markdown.contains("tokens: 1500"), "{markdown}");
    assert!(markdown.contains(&format!("({root})")) && !markdown.contains(&format!("({quiet})")), "teams without activity in the window are omitted");
    assert!(markdown.contains("- Goal: Build a stone wall here (http://x) now"), "untrusted text is made Markdown-inert: {markdown}");
    assert!(markdown.contains("wall.blocks 25 (+15)"), "{markdown}");
    assert!(markdown.contains("Pathologies: not observed"), "{markdown}");
    // One team, from any of its chats.
    let one = brief(&state, Some(&worker), now()).unwrap();
    assert!(one.contains("· team ") && one.contains(&format!("({root})")));
    assert!(brief(&state, Some("missing"), now()).is_err());
}

#[test]
fn notices_are_opt_in_rate_limited_and_never_repeated() {
    let mut state = State::default();
    let (root, _) = team(&mut state, "Watch");
    let at = 10_000;
    assert!(notify(&mut state, &root, None, "Team stuck", "n1", at).unwrap_err().contains("opt-in"));
    state.chats.iter_mut().find(|chat| chat.id == root).unwrap().lenses = Some(vec!["antibody".into()]);
    assert!(notify(&mut state, &root, Some("missing"), "Team stuck", "n1", at).is_err(), "a named team must be a top-level chat");
    let first = notify(&mut state, &root, Some(&root), "Team stuck", "n1", at).unwrap();
    assert_eq!(notify(&mut state, &root, None, "Team stuck", "n1", at).unwrap(), first, "a retried request returns the same notice");
    assert!(notify(&mut state, &root, None, "Team stuck", "n2", at + 1).unwrap_err().contains("never repeated"));
    assert!(notify(&mut state, &root, None, " ", "n3", at).is_err());
    state.runtime.execution.operator_notices_per_hour = 2;
    notify(&mut state, &root, None, "Second", "n4", at + 2).unwrap();
    assert!(notify(&mut state, &root, None, "Third", "n5", at + 3).unwrap_err().contains("limit"));
    notify(&mut state, &root, None, "Third", "n6", at + 3_601).unwrap();
    state.runtime.execution.operator_notices_per_hour = 0;
    assert!(notify(&mut state, &root, None, "Off", "n7", at + 9_000).is_err(), "0 disables notices");
}

#[test]
fn notices_show_as_pending_in_their_tree_for_a_day() {
    let mut state = State::default();
    let (root, worker) = team(&mut state, "Watch");
    state.chats.iter_mut().find(|chat| chat.id == root).unwrap().lenses = Some(vec!["antibody".into()]);
    notify(&mut state, &worker, None, "Member saw a loop", "n1", now()).unwrap();
    let pending = pending_json_tree(&state, &root);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["text"], "Member saw a loop");
    let requests = crate::chat::pending_requests(&state);
    let group = requests.as_array().unwrap().iter().find(|group| group["chat_id"] == root.as_str()).unwrap();
    assert_eq!(group["pending_notices"][0]["notice_id"], "n1");
    assert_eq!(control::client::operator::pending(group).iter().filter(|p| p.kind == control::client::operator::Kind::Notice).count(), 1);
    state.bots.notices[0].unix = now() - NOTICE_SECONDS - 1;
    assert!(pending_json_tree(&state, &root).is_empty() && crate::chat::pending_requests(&state).as_array().unwrap().is_empty());
}

#[test]
fn the_operator_call_saves_the_brief_under_the_state_directory() {
    let mut state = State::default();
    team(&mut state, "Build");
    let value = tool(&state, &json!({"save":true})).unwrap();
    let path = value["path"].as_str().unwrap();
    assert!(path.ends_with(&format!("intel/brief-{}.md", value["date"].as_str().unwrap())));
    assert_eq!(std::fs::read_to_string(path).unwrap(), value["markdown"].as_str().unwrap());
    assert!(tool(&state, &json!({})).unwrap()["path"].is_null(), "reading does not write");
    assert!(tool(&state, &json!({"chat":"../x"})).is_err());
}

/// A member chat of `root` whose one turn started `ago` seconds before now.
fn member(state: &mut State, root: &str, ago: u64) -> String {
    let chat = crate::chat::create(state);
    state.chats.iter_mut().find(|session| session.id == chat).unwrap().parent_chat = Some(root.to_owned());
    crate::chat::send(state, &chat, "Work", "test").unwrap();
    let run = state.runtime.runs.last_mut().unwrap();
    run.status = "completed".into();
    run.id = format!("{:x}-t", u128::from(now() - ago - 40) * 1_000_000_000);
    run.attempts = vec![crate::usage::Attempt { started_unix: now() - ago, elapsed_ms: 90_000,
        tokens: Some(control_ai::Tokens { input_tokens: 5_000, cached_input_tokens: 4_000, output_tokens: 500 }), ..Default::default() }];
    chat
}
fn claim(state: &mut State, owner: &str, key: &str, status: &str) {
    state.collective.claims.push(serde_json::from_value(json!({"room":"general","key":key,"owner":owner,"status":status,"revision":1})).unwrap());
}

#[test]
fn a_team_brief_lists_each_member_s_activity() {
    let mut state = State::default();
    let (root, _) = team(&mut state, "Build a castle");
    let active = member(&mut state, &root, 60);
    crate::chat::send(&mut state, &active, "Next", "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = "running".into();
    claim(&mut state, &active, "wall", "working");
    let holder = member(&mut state, &root, 900);
    claim(&mut state, &holder, "gate", "working");
    // A peer's message is retained as a completed run with no attempt: not a turn.
    crate::chat::send(&mut state, &holder, "Peer note", "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = "completed".into();
    let free = member(&mut state, &root, 1_200);
    claim(&mut state, &free, "roof", "done");
    state.watches.push(serde_json::from_value(json!({"id":"w","name":"n","chat":free,"service":"s","generation":1,"permanent":true,"command":"c","stopped":false})).unwrap());
    let markdown = brief(&state, Some(&root), now()).unwrap();
    let line = |chat: &str| {
        let name = crate::callsigns::name_or_id(&state.chats, chat).into_owned();
        markdown.lines().find(|line| line.starts_with(&format!("  - {name} ("))).unwrap_or_else(|| panic!("{name}: {markdown}")).to_owned()
    };
    assert!(line(&root).contains("(lead): 0 turns · no turn · idle · no claim"), "{markdown}");
    assert_eq!(line(&active), format!("  - {} (helper): 1 turns · 1m ago · running · wall working · 90s/40s · 1.5k tok", crate::callsigns::name_or_id(&state.chats, &active)));
    assert!(line(&holder).contains(": 1 turns · 15m ago · idle · gate working · 90s/40s"), "a peer message is not a turn: {markdown}");
    assert!(line(&free).contains(": 1 turns · 20m ago · idle · no claim, 1 done · 1 routine(s) ·"), "{markdown}");
    // The lead first, then the most recently active.
    let order = [&root, &active, &holder, &free].map(|chat| markdown.find(&line(chat)).unwrap());
    assert!(order.is_sorted(), "{markdown}");
    // It stays small: models read it on every patrol.
    claim(&mut state, &free, "a-long-deliverable-name-for-the-north-tower", "working");
    member(&mut state, &root, 300);
    let markdown = brief(&state, Some(&root), now()).unwrap();
    let table = markdown.lines().filter(|line| line.starts_with("- Per member") || line.starts_with("  - ")).map(|line| line.len() + 1).sum::<usize>();
    assert!(markdown.matches("\n  - ").count() == 5 && table < 1_200, "{table} bytes: {markdown}");
    // A helper with nothing live is a count, not a row.
    assert!(markdown.contains("\n  - 2 finished helper(s): 3.0k tok"), "{markdown}");
}
