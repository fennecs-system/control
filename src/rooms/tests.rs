use super::*;
use crate::runtime::RunStatus;
fn active(state: &mut State, parent: Option<&str>) -> (String, String) {
    let chat = crate::chat::create(state);
    state.chats.last_mut().unwrap().parent_chat = parent.map(str::to_owned);
    crate::chat::send(state, &chat, "Work", "test").unwrap();
    let run = &mut **state.runtime.runs.last_mut().unwrap();
    run.status = RunStatus::Running;
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some(format!("attempt-{}", run.id())),
        ..Default::default()
    });
    (chat, run.id().to_string())
}
fn room_call(state: &mut State, run: &str, tool: &str, args: Value, nonce: &str) -> Value {
    let response = checkpoint(
        state,
        run,
        &json!({"nonce":nonce,"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}}}),
        |_| Ok(()),
    );
    assert!(
        response.get("error").is_none() && response["result"]["isError"] == false,
        "{response}"
    );
    serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn task_threads_isolate_history_participants_and_inboxes_without_timers() {
    let mut state = State::default();
    tool(
        &mut state,
        "control.room.post",
        &json!({"room":"general","message":"old shared history"}),
    )
    .unwrap();
    let (lead, lead_run) = active(&mut state, None);
    // Children spawned before the lead links a room must resolve the same thread.
    let (child, child_run) = active(&mut state, Some(&lead));
    let (other, other_run) = active(&mut state, None);
    room_call(
        &mut state,
        &child_run,
        "control_room_post",
        json!({"room":"general","message":"child ready"}),
        "1-1",
    );
    room_call(
        &mut state,
        &lead_run,
        "control_room_post",
        json!({"room":"general","message":"our plan"}),
        "1-2",
    );
    room_call(
        &mut state,
        &other_run,
        "control_room_post",
        json!({"room":"general","message":"unrelated plan"}),
        "1-3",
    );
    let thread = format!("general/{lead}");
    let view = room_call(
        &mut state,
        &child_run,
        "control_room_read",
        json!({"room":"general"}),
        "1-4",
    );
    assert_eq!(view["room"], thread);
    assert_eq!(view["posts"].as_array().unwrap().len(), 2);
    assert_eq!(view["chats"].as_array().unwrap().len(), 2);
    assert!(!view.to_string().contains("unrelated plan") && !view.to_string().contains("old shared history"));
    let listed = room_call(&mut state, &other_run, "control_room_list", json!({}), "1-5");
    assert_eq!(listed["rooms"][0]["posts"], 1);
    assert_eq!(listed["rooms"][0]["room"], format!("general/{other}"));
    let foreign = worker_rpc(
        &mut state,
        &other_run,
        &json!({"nonce":"1-6","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_read","arguments":{"room":thread}}}}),
    );
    assert!(crate::toolbox::failure_text(&foreign).unwrap().contains("another task"));
    // An operator targets a thread explicitly; only that thread's inbox sees it.
    tool(
        &mut state,
        "control.room.post",
        &json!({"room":thread,"message":"operator direction"}),
    )
    .unwrap();
    let inbox = notifications::call(&mut state, &child_run, "control_room_notifications", &json!({})).unwrap();
    assert!(inbox.to_string().contains("our plan") && inbox.to_string().contains("operator direction"));
    assert!(!inbox.to_string().contains("unrelated plan"));
    let other_inbox = notifications::call(&mut state, &other_run, "control_room_notifications", &json!({})).unwrap();
    assert!(!other_inbox.to_string().contains("operator direction"));
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(address::for_worker(&restored, &child, "general").unwrap(), thread);
    let page = render(&restored, &thread);
    assert!(page.contains(&format!("name='channel' value='{thread}'")) && page.contains("operator direction"));
    assert!(!page.contains("unrelated plan") && !page.contains("old shared history"));
    assert!(page.contains("name='lens' value='none'"), "thread IDs are not lens IDs");
    // Isolate room-triggered wakes from the separate enable-bot bootstrap
    // for an attempt with an unpublished handoff.
    for run in state.runtime.runs.iter_mut() {
        run.status = RunStatus::Completed;
        run.attempts.clear();
    }
    crate::bots::command(&mut state, &child, "on").unwrap();
    crate::bots::command(&mut state, &other, "on").unwrap();
    tool(
        &mut state,
        "control.room.post",
        &json!({"room":thread,"message":"new dependency ready"}),
    )
    .unwrap();
    crate::bots::reconcile(&mut state, |_| Ok(())).unwrap();
    let wakes: Vec<_> = state
        .runtime
        .runs
        .iter()
        .filter(|run| run.bot_turn.is_some())
        .map(|run| run.chat_id.as_deref())
        .collect();
    assert_eq!(
        wakes,
        vec![Some(child.as_str())],
        "only the intended task wakes, without a timer"
    );
}

#[test]
fn operator_selected_threads_are_inherited_and_legacy_links_stay_shared() {
    let mut state = State::default();
    let (lead, _) = active(&mut state, None);
    let (joiner, _) = active(&mut state, None);
    state.chats.last_mut().unwrap().room = Some(format!("engineering/{lead}"));
    let (child, _) = active(&mut state, Some(&joiner));
    assert_eq!(
        address::for_worker(&state, &child, "general").unwrap(),
        format!("general/{lead}")
    );
    state.chats.iter_mut().find(|chat| chat.id == joiner).unwrap().room = Some("engineering".into());
    assert_eq!(address::for_worker(&state, &child, "general").unwrap(), "general");
    for invalid in [
        "general/",
        "general/../../secret",
        "general/a/b",
        "lobby/a",
        "general/<script>",
    ] {
        assert!(!valid(invalid));
    }
}

#[test]
fn room_sidebar_scopes_activity_and_keeps_message_chat_links() {
    let mut state = State::default();
    let active = crate::chat::create(&mut state);
    state.chats[0].room = Some("research".into());
    crate::chat::send(&mut state, &active, "Investigate", "test").unwrap();
    state.chats[0].name = Some("Investigate <sources>".into());
    state.runtime.runs[0].status = RunStatus::Running;
    let idle = crate::chat::create(&mut state);
    state.chats[1].room = Some("research".into());
    state.chats[1].title = "Previous findings".into();
    let other = crate::chat::create(&mut state);
    state.chats[2].room = Some("engineering".into());
    state.collective.posts.push(Post {
        room: "research".into(),
        text: "An update <script>".into(),
        chat: Some(active.clone()),
        run: Some(state.runtime.runs[0].id().to_string()),
        key: String::new(),
        delivery: Default::default(),
    });
    let page = crate::render(&state, "rooms", "none", "research");
    let (_, sidebar) = page.split_once("aria-label='Room details'").unwrap();
    let (participants, links) = sidebar.split_once("<h2>Linked DMs</h2>").unwrap();
    assert!(page.contains("body-grid rooms-workspace"));
    assert!(page.contains("channel=research' aria-current='page'"));
    assert!(page.contains("An update &lt;script&gt;"));
    assert!(page.contains(&format!("href='/dm/{active}'>Open DM ↗")));
    assert!(participants.contains("Investigate &lt;sources&gt;"));
    assert!(participants.contains("<small>running</small>"));
    assert!(!participants.contains(&format!("href='/dm/{idle}'")));
    assert!(links.contains(&format!("href='/dm/{idle}'")));
    assert!(!links.contains(&format!("href='/dm/{active}'")));
    assert!(!sidebar.contains(&format!("href='/dm/{other}'")));
    let empty = render(&state, "unknown");
    assert!(empty.contains("aria-label='#general'"));
    assert!(empty.contains("No active participants."));
    assert!(empty.contains("No updates yet."));
}
#[test]
fn worker_is_scoped_and_cannot_forge_authority_or_duplicate_posts() {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    state.chats[0].room = Some("research".into());
    crate::chat::send(&mut state, &id, "Investigate a source", "test").unwrap();
    state.runtime.runs[0].status = RunStatus::Running;
    let run = state.runtime.runs[0].id().to_string();
    let req = json!({"nonce":"123-1","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"engineering","actor":"architect","message":"<shared>"}}}});
    assert_eq!(worker_rpc(&mut state, &run, &req)["result"]["isError"], false);
    worker_rpc(&mut state, &run, &req);
    assert_eq!(state.collective.posts.len(), 1);
    assert_eq!(
        (state.collective.posts[0].room.as_str(), state.chats[0].room.as_deref()),
        ("engineering", Some("research"))
    );
    assert_eq!(
        state.collective.posts[0].run.as_deref(),
        Some(run.as_str()),
        "author comes from the run"
    );
    assert!(render(&state, "engineering").contains("&lt;shared&gt;"));
    let forbidden =
        json!({"request":{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"control.chat.send"}}});
    let denied = worker_rpc(&mut state, &run, &forbidden);
    assert_eq!(
        crate::toolbox::failure_text(&denied),
        Some("Tool is not allowed for this worker."),
        "{denied}"
    );
    assert_eq!(denied["id"], 2);
    state.runtime.runs[0].status = RunStatus::Completed;
    assert_eq!(
        crate::toolbox::failure_text(&worker_rpc(&mut state, &run, &req)),
        Some("Run is not active.")
    );
}
#[test]
fn delivery_is_explicit_durable_and_cannot_be_changed_by_retry() {
    let mut state = State::default();
    let (_, run) = active(&mut state, None);
    let first = room_call(
        &mut state,
        &run,
        "control_room_post",
        json!({"room":"general","message":"Audit retained","delivery":"record"}),
        "1-1",
    );
    assert_eq!(first["delivery"], "record");
    state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    let retry = room_call(
        &mut state,
        &run,
        "control_room_post",
        json!({"room":"general","message":"Changed retry","delivery":"notify"}),
        "1-1",
    );
    assert_eq!(retry["delivery"], "record");
    assert_eq!(state.collective.posts.len(), 1);
    assert_eq!(state.collective.posts[0].text, "Audit retained");
    let invalid = worker_rpc(
        &mut state,
        &run,
        &json!({"nonce":"1-2","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_post","arguments":{"message":"Bad","delivery":"maybe"}}}}),
    );
    assert_eq!(
        crate::toolbox::failure_text(&invalid),
        Some("Expected delivery=record or notify."),
        "{invalid}"
    );
    assert_eq!(invalid["id"], 1);
    assert_eq!(state.collective.posts.len(), 1);
    let legacy = room_call(
        &mut state,
        &run,
        "control_room_post",
        json!({"message":"Dependency ready"}),
        "1-3",
    );
    assert_eq!(legacy["delivery"], "notify");
    let old: Post =
        serde_json::from_value(json!({"room":"general","text":"Old state","chat":null,"run":null,"key":"old"}))
            .unwrap();
    assert_eq!(old.delivery, Delivery::Notify);
    tool(
        &mut state,
        "control.room.post",
        &json!({"room":"general","message":"Human steering","delivery":"record"}),
    )
    .unwrap();
    assert_eq!(
        state.collective.posts.last().unwrap().delivery,
        Delivery::Notify,
        "operator input always notifies"
    );
}

#[test]
fn unlinked_chats_discover_rooms_and_posting_links_them() {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &id, "Coordinate", "test").unwrap();
    state.runtime.runs[0].status = RunStatus::Running;
    let run = state.runtime.runs[0].id().to_string();
    let call = |name: &str, args: Value, nonce: &str| json!({"nonce":nonce,"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}});
    let text = |response: Value| {
        serde_json::from_str::<Value>(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    let names = worker_rpc(
        &mut state,
        &run,
        &json!({"request":{"jsonrpc":"2.0","id":1,"method":"tools/list"}}),
    )["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(names.iter().any(|n| n == "control_room_list"), "{names:?}");
    let list = text(worker_rpc(
        &mut state,
        &run,
        &call("control_room_list", json!({}), "1-1"),
    ));
    let thread = format!("general/{id}");
    assert_eq!(
        (list["attached"].clone(), list["rooms"].as_array().unwrap().len()),
        (json!(thread), NAMES.len()),
        "an unlinked task defaults to its own #general thread"
    );
    let empty = text(worker_rpc(
        &mut state,
        &run,
        &call("control_room_read", json!({}), "1-2"),
    ));
    assert_eq!(
        (&empty["room"], &empty["note"]),
        (
            &json!(thread),
            &json!("No posts, claims or linked chats in this room yet.")
        )
    );
    assert_eq!(state.chats[0].room, None, "reading links nothing");
    let unknown = worker_rpc(
        &mut state,
        &run,
        &call("control_room_read", json!({"room":"lobby"}), "1-3"),
    );
    assert_eq!(
        crate::toolbox::failure_text(&unknown),
        Some(format!("Unknown room lobby. This chat's team room is {thread}; omit room to use it.").as_str())
    );
    let failed = checkpoint(
        &mut state,
        &run,
        &call("control_room_post", json!({"room":"general","message":"hi"}), "1-4"),
        |_| Err(std::io::Error::other("disk")),
    );
    assert_eq!(
        crate::toolbox::failure_text(&failed),
        Some("Could not save room update. Try again.")
    );
    assert_eq!(
        (state.chats[0].room.as_deref(), state.collective.posts.len()),
        (None, 0),
        "a failed save unlinks and drops the post"
    );
    checkpoint(
        &mut state,
        &run,
        &call("control_room_post", json!({"room":"general","message":"hi"}), "1-5"),
        |_| Ok(()),
    );
    assert_eq!(state.chats[0].room.as_deref(), Some(thread.as_str()));
    checkpoint(
        &mut state,
        &run,
        &call("control_room_post", json!({"room":"research","message":"also"}), "1-6"),
        |_| Ok(()),
    );
    assert_eq!(
        state.chats[0].room.as_deref(),
        Some(thread.as_str()),
        "posting elsewhere keeps the link"
    );
    assert_eq!(
        text(worker_rpc(
            &mut state,
            &run,
            &call("control_room_read", json!({}), "1-7")
        ))["room"],
        thread
    );
}
#[test]
fn a_bot_whose_chat_links_by_posting_hears_that_room() {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &id, "Keep watch", "test").unwrap();
    crate::bots::command(&mut state, &id, "on every 10").unwrap();
    state.runtime.runs[0].status = RunStatus::Running;
    let run = state.runtime.runs[0].id().to_string();
    let post = json!({"nonce":"2-1","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"research","message":"here"}}}});
    let failed = checkpoint(&mut state, &run, &post, |_| Err(std::io::Error::other("disk")));
    assert!(crate::toolbox::failure_text(&failed).is_some(), "{failed}");
    assert!(
        !crate::bots::status(&state, &id).unwrap().contains("research"),
        "a failed save keeps the bot unchanged"
    );
    checkpoint(&mut state, &run, &post, |_| Ok(()));
    assert!(crate::bots::status(&state, &id).unwrap().contains("room #research"));
    assert!(
        state.collective.room_watches.iter().any(|w| w.chat == id),
        "the bot reads replies from an inbox"
    );
}
#[test]
fn mailbox_rejects_symlinks_and_oversized_requests() {
    let dir = crate::test_support::scratch("control-room-test");
    fs::create_dir_all(&dir).unwrap();
    let link = dir.join("link");
    std::os::unix::fs::symlink("/etc/hosts", &link).unwrap();
    assert!(read_request(&link).is_err());
    fs::write(dir.join("large"), vec![b' '; 32769]).unwrap();
    assert!(read_request(&dir.join("large")).is_err());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_late_reply_cannot_remove_the_next_mailbox_request() {
    let dir = crate::test_support::scratch("control-room-late-reply");
    fs::create_dir_all(&dir).unwrap();
    let first = json!({"nonce":"1","request":{"id":7}});
    let next = json!({"nonce":"2","request":{"id":7}});
    fs::write(dir.join(".control-room-request"), first.to_string()).unwrap();
    assert_eq!(take_request(&dir).unwrap(), first);
    fs::write(dir.join(".control-room-request"), next.to_string()).unwrap();
    reply(&dir, &json!({"nonce":"1","response":{"id":7,"result":{}}}));
    assert_eq!(
        take_request(&dir).unwrap(),
        next,
        "a late reply must not discard a newer delivery, even if JSON-RPC IDs are reused"
    );
    assert!(take_request(&dir).is_err());
    fs::remove_dir_all(dir).unwrap();
}

fn room_failure(state: &mut State, run: &str, tool: &str, args: Value) -> String {
    let response = worker_rpc(
        state,
        run,
        &json!({"nonce":"9-9","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}}}),
    );
    crate::toolbox::failure_text(&response).unwrap().to_owned()
}

#[test]
fn omitted_room_uses_the_team_room_and_unknown_names_point_to_it() {
    let mut state = State::default();
    let (lead, _) = active(&mut state, None);
    let team = format!("research/{lead}");
    state.chats[0].room = Some(team.clone());
    // A worker started before or without inheriting a link still finds its team.
    let (child, child_run) = active(&mut state, Some(&lead));
    assert_eq!(state.chats[1].room, None);
    assert_eq!(
        room_call(&mut state, &child_run, "control_room_list", json!({}), "2-1")["attached"],
        team
    );
    assert_eq!(
        room_call(&mut state, &child_run, "control_room_read", json!({}), "2-2")["room"],
        team
    );
    // The prompt renders it as #research/<lead>; that spelling names it too.
    assert_eq!(
        room_call(
            &mut state,
            &child_run,
            "control_room_read",
            json!({"room":format!("#{team}")}),
            "2-5"
        )["room"],
        team
    );
    assert_eq!(
        room_call(
            &mut state,
            &child_run,
            "control_room_read",
            json!({"room":"#research"}),
            "2-6"
        )["room"],
        team
    );
    let claim = room_call(
        &mut state,
        &child_run,
        "control_room_claim",
        json!({"key":"m1","action":"acquire"}),
        "2-3",
    );
    assert_eq!(claim["applied"], true, "{claim}");
    assert_eq!(state.collective.claims[0].room, team);
    // A mistaken name fails visibly and names the team room instead of redirecting.
    let error = room_failure(
        &mut state,
        &child_run,
        "control_room_post",
        json!({"room":"castle","message":"joined"}),
    );
    assert_eq!(
        error,
        format!("Unknown room castle. This chat's team room is {team}; omit room to use it.")
    );
    assert!(state.collective.posts.is_empty() && state.chats[1].room.is_none());
    let posted = room_call(
        &mut state,
        &child_run,
        "control_room_post",
        json!({"message":"joined"}),
        "2-4",
    );
    assert_eq!(posted["room"], team);
    assert_eq!(
        state
            .chats
            .iter()
            .find(|chat| chat.id == child)
            .unwrap()
            .room
            .as_deref(),
        Some(team.as_str()),
        "posting links it to hear replies"
    );
    // Neither an unrelated task nor another actor's descendant inherits the link;
    // an unlinked task defaults to its own #general thread.
    let (other, other_run) = active(&mut state, None);
    assert_eq!(
        room_call(&mut state, &other_run, "control_room_read", json!({}), "2-7")["room"],
        format!("general/{other}")
    );
    assert_eq!(
        room_failure(
            &mut state,
            &other_run,
            "control_room_post",
            json!({"room":"castle","message":"x"})
        ),
        format!("Unknown room castle. This chat's team room is general/{other}; omit room to use it.")
    );
    let (_, foreign_run) = active(&mut state, Some(&lead));
    state.chats[0].actor = crate::identity::Actor::Unknown;
    assert!(room_failure(&mut state, &foreign_run, "control_room_read", json!({})).contains("No room attached"));
}

#[test]
fn an_unlinked_team_shares_its_general_thread_for_claim_read_and_post() {
    // A root chat started without a room, with a worker spawned before anyone posts.
    let mut state = State::default();
    let (lead, lead_run) = active(&mut state, None);
    let (child, child_run) = active(&mut state, Some(&lead));
    let team = format!("general/{lead}");
    let claim = room_call(
        &mut state,
        &lead_run,
        "control_room_claim",
        json!({"key":"server","action":"acquire"}),
        "4-1",
    );
    assert_eq!(
        (claim["applied"].clone(), state.collective.claims[0].room.as_str()),
        (json!(true), team.as_str()),
        "{claim}"
    );
    assert_eq!(
        room_call(
            &mut state,
            &lead_run,
            "control_room_post",
            json!({"message":"backlog"}),
            "4-2"
        )["room"],
        team
    );
    assert_eq!(
        state.chats[0].room.as_deref(),
        Some(team.as_str()),
        "posting links the lead"
    );
    let read = room_call(&mut state, &child_run, "control_room_read", json!({}), "4-3");
    assert_eq!(
        (&read["room"], &read["posts"][0]["text"], &read["claims"][0]["key"]),
        (&json!(team), &json!("backlog"), &json!("server"))
    );
    let list = room_call(&mut state, &child_run, "control_room_list", json!({}), "4-4");
    assert_eq!(
        (
            &list["attached"],
            &list["rooms"][0]["posts"],
            &list["rooms"][0]["chats"]
        ),
        (&json!(team), &json!(1), &json!(1))
    );
    assert_eq!(
        state.chats.iter().find(|chat| chat.id == child).unwrap().room,
        None,
        "reading links nothing"
    );
}

#[test]
fn an_advisor_does_not_post_through_its_inherited_team_room() {
    let mut state = State::default();
    let lead = crate::chat::create(&mut state);
    state.chats[0].lenses = Some(vec!["advisor".into()]);
    state.chats[0].room = Some(format!("general/{lead}"));
    crate::chat::send(&mut state, &lead, "Lead the team", "test").unwrap();
    state.runtime.runs[0].status = RunStatus::Completed;
    let (advisor, run) = active(&mut state, Some(&lead));
    let session = state.runtime.runs.get_mut(&run).unwrap();
    session.persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Judge,
        root: "root".into(),
        depth: 1,
        parent: None,
        bundle: None,
    });
    crate::bots::command(&mut state, &advisor, "on").unwrap();
    assert!(state.bots.advises(&advisor));
    // The room tells members whom to ask, though the advisor is not linked to it.
    assert_eq!(
        read(&state, &format!("general/{lead}"))["advisor"]["id"],
        advisor.as_str()
    );
    assert!(read(&state, "general/another")["advisor"].is_null());
    assert!(
        room_failure(
            &mut state,
            &run,
            "control_room_post",
            json!({"message":"Build claim looks abandoned"})
        )
        .contains("does not post to rooms")
    );
    assert!(state.collective.posts.is_empty());
    assert_eq!(state.chats[1].room, None, "a refused post links nothing");
}

#[test]
fn the_room_plan_follows_room_access_rules() {
    let mut state = State::default();
    let (lead, lead_run) = active(&mut state, None);
    let team = format!("general/{lead}");
    state.chats[0].room = Some(team.clone());
    let (_, child_run) = active(&mut state, Some(&lead));
    room_call(
        &mut state,
        &child_run,
        "control_room_claim",
        json!({"key":"axe","action":"acquire"}),
        "3-1",
    );
    let plan = room_call(&mut state, &lead_run, "control_room_plan", json!({}), "3-2");
    assert_eq!((&plan["room"], &plan["critical_path"]), (&json!(team), &json!(["axe"])));
    assert!(state.collective.claim_seq == 1, "planning changes nothing");
    let (other, other_run) = active(&mut state, None);
    assert!(room_failure(&mut state, &other_run, "control_room_plan", json!({"room":team})).contains("another task"));
    assert_eq!(
        room_call(&mut state, &other_run, "control_room_plan", json!({}), "3-3")["room"],
        format!("general/{other}")
    );
}
/// Pins the room server's envelopes: method checks come before the run's
/// authority, and every other refusal is -32602 outside tools/call.
#[test]
fn room_envelopes_are_pinned() {
    let mut state = State::default();
    let request = |request: Value| json!({"nonce":"1","request":request});
    for (envelope, expected) in [
        (
            request(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
            Value::Null,
        ),
        (
            request(json!({"jsonrpc":"1.0","id":1,"method":"ping"})),
            json!({"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid JSON-RPC request"}}),
        ),
        (
            request(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{}})),
            json!({"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"tools/call requires params.name"}}),
        ),
        (
            request(json!({"jsonrpc":"2.0","id":3,"method":"prompts/list"})),
            json!({"jsonrpc":"2.0","id":3,"error":{"code":-32601,"message":"Unsupported room method."}}),
        ),
        (
            request(json!({"jsonrpc":"2.0","id":4,"method":"initialize"})),
            json!({"jsonrpc":"2.0","id":4,"error":{"code":-32602,"message":"Run is not active."}}),
        ),
        (
            request(json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"control_room_list"}})),
            json!({"jsonrpc":"2.0","id":5,"result":{"content":[{"type":"text","text":"Run is not active."}],"isError":true}}),
        ),
    ] {
        assert_eq!(worker_rpc(&mut state, "missing", &envelope), expected, "{envelope}");
    }
    let (_, run) = active(&mut state, None);
    let initialized = worker_rpc(
        &mut state,
        &run,
        &request(json!({"jsonrpc":"2.0","id":6,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})),
    );
    assert_eq!(
        initialized,
        json!({"jsonrpc":"2.0","id":6,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"control-room","version":"0.1"}}})
    );
    assert_eq!(
        worker_rpc(
            &mut state,
            &run,
            &request(json!({"jsonrpc":"2.0","id":7,"method":"ping"}))
        ),
        json!({"jsonrpc":"2.0","id":7,"result":{}})
    );
    let listed = worker_rpc(
        &mut state,
        &run,
        &request(json!({"jsonrpc":"2.0","id":8,"method":"tools/list"})),
    );
    assert_eq!(
        listed,
        json!({"jsonrpc":"2.0","id":8,"result":{"tools":worker_definitions(false)}})
    );
    let unknown = worker_rpc(
        &mut state,
        &run,
        &request(json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"control_nope"}})),
    );
    assert_eq!(
        unknown,
        json!({"jsonrpc":"2.0","id":9,"result":{"content":[{"type":"text","text":"Tool is not allowed for this worker."}],"isError":true}})
    );
    let listed = worker_rpc(
        &mut state,
        &run,
        &request(json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"control_room_list"}})),
    );
    assert_eq!(
        listed["result"].as_object().unwrap().len(),
        2,
        "room results carry no structuredContent: {listed}"
    );
    assert_eq!(listed["result"]["isError"], false);
}
