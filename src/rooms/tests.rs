
use super::*;
fn active(state: &mut State, parent: Option<&str>) -> (String, String) {
    let chat = crate::chat::create(state);
    state.chats.last_mut().unwrap().parent_chat = parent.map(str::to_owned);
    crate::chat::send(state, &chat, "Work", "test").unwrap();
    let run = state.runtime.runs.last_mut().unwrap();
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt {
        workspace_id: Some(format!("attempt-{}", run.id)),
        ..Default::default()
    });
    (chat, run.id.clone())
}
fn room_call(state: &mut State, run: &str, tool: &str, args: Value, nonce: &str) -> Value {
    let response = checkpoint(
        state,
        run,
        &json!({"nonce":nonce,"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}}}),
        |_| Ok(()),
    );
    assert!(response.get("error").is_none(), "{response}");
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
    assert!(
        !view.to_string().contains("unrelated plan")
            && !view.to_string().contains("old shared history")
    );
    let listed = room_call(
        &mut state,
        &other_run,
        "control_room_list",
        json!({}),
        "1-5",
    );
    assert_eq!(listed["rooms"][0]["posts"], 1);
    assert_eq!(listed["rooms"][0]["room"], format!("general/{other}"));
    let foreign = worker_rpc(
        &mut state,
        &other_run,
        &json!({"nonce":"1-6","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_read","arguments":{"room":thread}}}}),
    );
    assert!(
        foreign["error"]["message"]
            .as_str()
            .unwrap()
            .contains("another task")
    );
    // An operator targets a thread explicitly; only that thread's inbox sees it.
    tool(
        &mut state,
        "control.room.post",
        &json!({"room":thread,"message":"operator direction"}),
    )
    .unwrap();
    let inbox = notifications::call(
        &mut state,
        &child_run,
        "control_room_notifications",
        &json!({}),
    )
    .unwrap();
    assert!(
        inbox.to_string().contains("our plan") && inbox.to_string().contains("operator direction")
    );
    assert!(!inbox.to_string().contains("unrelated plan"));
    let other_inbox = notifications::call(
        &mut state,
        &other_run,
        "control_room_notifications",
        &json!({}),
    )
    .unwrap();
    assert!(!other_inbox.to_string().contains("operator direction"));
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(
        address::for_worker(&restored, &child, "general").unwrap(),
        thread
    );
    let page = render(&restored, &thread);
    assert!(
        page.contains(&format!("name='channel' value='{thread}'"))
            && page.contains("operator direction")
    );
    assert!(!page.contains("unrelated plan") && !page.contains("old shared history"));
    assert!(
        page.contains("name='lens' value='none'"),
        "thread IDs are not lens IDs"
    );
    // Isolate room-triggered wakes from the separate enable-bot bootstrap
    // for an attempt with an unpublished handoff.
    for run in &mut state.runtime.runs {
        run.status = "completed".into();
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
    state
        .chats
        .iter_mut()
        .find(|chat| chat.id == joiner)
        .unwrap()
        .room = Some("engineering".into());
    assert_eq!(
        address::for_worker(&state, &child, "general").unwrap(),
        "general"
    );
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
    state.chats[0].title = "Investigate <sources>".into();
    state.runtime.runs[0].status = "running".into();
    let idle = crate::chat::create(&mut state);
    state.chats[1].room = Some("research".into());
    state.chats[1].title = "Previous findings".into();
    let other = crate::chat::create(&mut state);
    state.chats[2].room = Some("engineering".into());
    state.collective.posts.push(Post {
        room: "research".into(),
        text: "An update <script>".into(),
        chat: Some(active.clone()),
        run: Some(state.runtime.runs[0].id.clone()),
        key: String::new(),
        delivery: Default::default(),
    });
    let page = crate::render(&state, "rooms", "none", "research");
    let (_, sidebar) = page.split_once("aria-label='Room details'").unwrap();
    let (participants, links) = sidebar.split_once("<h2>Linked chats</h2>").unwrap();
    assert!(page.contains("body-grid rooms-workspace"));
    assert!(page.contains("channel=research' aria-current='page'"));
    assert!(page.contains("An update &lt;script&gt;"));
    assert!(page.contains(&format!("href='/chat/{active}'>Open chat ↗")));
    assert!(participants.contains("Investigate &lt;sources&gt;"));
    assert!(participants.contains("<small>running</small>"));
    assert!(!participants.contains(&format!("href='/chat/{idle}'")));
    assert!(links.contains(&format!("href='/chat/{idle}'")));
    assert!(!links.contains(&format!("href='/chat/{active}'")));
    assert!(!sidebar.contains(&format!("href='/chat/{other}'")));
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
    state.runtime.runs[0].status = "running".into();
    let run = state.runtime.runs[0].id.clone();
    let req = json!({"nonce":"123-1","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"engineering","actor":"architect","message":"<shared>"}}}});
    assert_eq!(
        worker_rpc(&mut state, &run, &req)["result"]["isError"],
        false
    );
    worker_rpc(&mut state, &run, &req);
    assert_eq!(state.collective.posts.len(), 1);
    assert_eq!(
        (
            state.collective.posts[0].room.as_str(),
            state.chats[0].room.as_deref()
        ),
        ("engineering", Some("research"))
    );
    assert_eq!(
        state.collective.posts[0].run.as_deref(),
        Some(run.as_str()),
        "author comes from the run"
    );
    assert!(render(&state, "engineering").contains("&lt;shared&gt;"));
    let forbidden =
        json!({"request":{"id":2,"method":"tools/call","params":{"name":"control.chat.send"}}});
    assert!(
        worker_rpc(&mut state, &run, &forbidden)
            .get("error")
            .is_some()
    );
    state.runtime.runs[0].status = "completed".into();
    assert!(worker_rpc(&mut state, &run, &req).get("error").is_some());
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
        &json!({"nonce":"1-2","request":{"id":1,"method":"tools/call","params":{"name":"control_room_post","arguments":{"message":"Bad","delivery":"maybe"}}}}),
    );
    assert!(invalid.get("error").is_some());
    assert_eq!(state.collective.posts.len(), 1);
    let legacy = room_call(
        &mut state,
        &run,
        "control_room_post",
        json!({"message":"Dependency ready"}),
        "1-3",
    );
    assert_eq!(legacy["delivery"], "notify");
    let old: Post = serde_json::from_value(
        json!({"room":"general","text":"Old state","chat":null,"run":null,"key":"old"}),
    )
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
    state.runtime.runs[0].status = "running".into();
    let run = state.runtime.runs[0].id.clone();
    let call = |name: &str, args: Value, nonce: &str| json!({"nonce":nonce,"request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}});
    let text = |response: Value| {
        serde_json::from_str::<Value>(response["result"]["content"][0]["text"].as_str().unwrap())
            .unwrap()
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
    assert_eq!(
        (
            list["attached"].clone(),
            list["rooms"].as_array().unwrap().len()
        ),
        (Value::Null, NAMES.len())
    );
    assert!(
        worker_rpc(
            &mut state,
            &run,
            &call("control_room_post", json!({"message":"hi"}), "1-2")
        )["error"]["message"]
            .as_str()
            .unwrap()
            .contains("control_room_list")
    );
    assert!(
        worker_rpc(
            &mut state,
            &run,
            &call("control_room_read", json!({"room":"lobby"}), "1-3")
        )
        .get("error")
        .is_some()
    );
    let failed = checkpoint(
        &mut state,
        &run,
        &call(
            "control_room_post",
            json!({"room":"general","message":"hi"}),
            "1-4",
        ),
        |_| Err(std::io::Error::other("disk")),
    );
    assert!(failed.get("error").is_some());
    assert_eq!(
        (state.chats[0].room.as_deref(), state.collective.posts.len()),
        (None, 0),
        "a failed save unlinks and drops the post"
    );
    checkpoint(
        &mut state,
        &run,
        &call(
            "control_room_post",
            json!({"room":"general","message":"hi"}),
            "1-5",
        ),
        |_| Ok(()),
    );
    let thread = format!("general/{id}");
    assert_eq!(state.chats[0].room.as_deref(), Some(thread.as_str()));
    checkpoint(
        &mut state,
        &run,
        &call(
            "control_room_post",
            json!({"room":"research","message":"also"}),
            "1-6",
        ),
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
    state.runtime.runs[0].status = "running".into();
    let run = state.runtime.runs[0].id.clone();
    let post = json!({"nonce":"2-1","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_room_post","arguments":{"room":"research","message":"here"}}}});
    assert!(
        checkpoint(&mut state, &run, &post, |_| Err(std::io::Error::other(
            "disk"
        )))
        .get("error")
        .is_some()
    );
    assert!(
        !crate::bots::status(&state, &id)
            .unwrap()
            .contains("research"),
        "a failed save keeps the bot unchanged"
    );
    checkpoint(&mut state, &run, &post, |_| Ok(()));
    assert!(
        crate::bots::status(&state, &id)
            .unwrap()
            .contains("room #research")
    );
    assert!(
        state.collective.room_watches.iter().any(|w| w.chat == id),
        "the bot reads replies from an inbox"
    );
}
#[test]
fn mailbox_rejects_symlinks_and_oversized_requests() {
    let dir = std::env::temp_dir().join(format!("control-room-test-{}", std::process::id()));
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
    let dir = std::env::temp_dir().join(format!("control-room-late-reply-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let first = json!({"nonce":"1","request":{"id":7}});
    let next = json!({"nonce":"2","request":{"id":7}});
    fs::write(dir.join(".control-room-request"), first.to_string()).unwrap();
    assert_eq!(take_request(&dir).unwrap(), first);
    fs::write(dir.join(".control-room-request"), next.to_string()).unwrap();
    reply(&dir, &json!({"nonce":"1","response":{"id":7,"result":{}}}));
    assert_eq!(take_request(&dir).unwrap(), next, "a late reply must not discard a newer delivery, even if JSON-RPC IDs are reused");
    assert!(take_request(&dir).is_err());
    fs::remove_dir_all(dir).unwrap();
}
