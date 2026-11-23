use super::*;
use control::permissions::Approvals;

/// An operator DM (`root`) whose agent runs in `own`, with an unrelated DM.
fn task() -> (Arc<Mutex<State>>, String, String, String) {
    let mut state = State::default();
    let root = crate::chat::create(&mut state);
    let own = crate::chat::create(&mut state);
    state.chats.last_mut().unwrap().parent_chat = Some(root.clone());
    let other = crate::chat::create(&mut state);
    (Arc::new(Mutex::new(state)), root, own, other)
}

#[test]
fn agents_drive_only_dms_below_their_own_chat() {
    let (shared, root, own, other) = task();
    let mut scoped = Scoped {
        shared: &shared,
        own: own.clone(),
    };
    let created = call(
        &mut scoped,
        "chat_create",
        &json!({"title":"Test DM"}),
        Approvals::default(),
    )
    .unwrap();
    let child = created
        .strip_prefix("Created chat ")
        .unwrap()
        .trim_end_matches('.')
        .to_owned();
    assert_eq!(
        shared
            .lock()
            .unwrap()
            .chats
            .iter()
            .find(|c| c.id == child)
            .unwrap()
            .parent_chat
            .as_deref(),
        Some(own.as_str())
    );

    call(
        &mut scoped,
        "chat_send",
        &json!({"id":child,"message":"Try the new tool"}),
        Approvals::default(),
    )
    .unwrap();
    assert!(
        call(&mut scoped, "chat_read", &json!({"id":child}), Approvals::default())
            .unwrap()
            .contains("> Try the new tool")
    );
    let listed = call(&mut scoped, "chats_list", &json!({}), Approvals::default()).unwrap();
    assert!(
        listed.contains(&child)
            && listed.contains(&own)
            && !listed.contains(&format!("{root} ["))
            && !listed.contains(&other),
        "{listed}"
    );
    assert!(
        call(&mut scoped, "chat_team", &json!({"id":own}), Approvals::default())
            .unwrap()
            .contains(&child)
    );
    assert!(
        call(&mut scoped, "chat_requests", &json!({"id":child}), Approvals::default())
            .unwrap()
            .starts_with("Nothing in task")
    );
    call(
        &mut scoped,
        "chat_cancel",
        &json!({"id":child,"all":true}),
        Approvals::default(),
    )
    .unwrap();

    // Never its parents, unrelated DMs or its own chat.
    for id in [&root, &other] {
        assert!(call(&mut scoped, "chat_read", &json!({"id":id}), Approvals::default()).is_err());
        assert!(
            call(
                &mut scoped,
                "chat_send",
                &json!({"id":id,"message":"hi"}),
                Approvals::default()
            )
            .is_err()
        );
    }
    assert!(
        call(
            &mut scoped,
            "chat_send",
            &json!({"id":own,"message":"hi"}),
            Approvals::default()
        )
        .is_err()
    );
    assert!(
        call(
            &mut scoped,
            "chat_cancel",
            &json!({"id":own,"all":true}),
            Approvals::default()
        )
        .is_err()
    );
    // No decisions or folders, even when the operator lets the external bridge decide.
    let all = Approvals {
        folder: true,
        port: true,
        bot: true,
        question: true,
        knowledge: true,
    };
    assert!(call(&mut scoped, "chat_open", &json!({"id":child,"paths":["/srv"]}), all).is_err());
    assert!(
        call(
            &mut scoped,
            "chat_decide",
            &json!({"id":child,"kind":"bot","request":"b-1","decision":"allow"}),
            all
        )
        .is_err()
    );
    assert!(
        call(
            &mut scoped,
            "chat_decide",
            &json!({"id":child,"kind":"knowledge","request":"claim:0","decision":"allow"}),
            all
        )
        .is_err()
    );
    assert!(shared.lock().unwrap().chats.iter().all(|c| c.project.is_none()));
}

#[test]
fn the_worker_tool_is_hidden_and_refused_unless_the_operator_enables_it() {
    let (shared, _, own, _) = task();
    assert!(Tools.definitions().is_empty());
    assert!(Tools.known("control_chat"));
    let host = crate::tool_registry::LocalHost(&shared);
    let cx = AttemptCall {
        attempt: crate::tool_registry::AttemptRef {
            run: "r-1",
            generation: "g",
            nonce: "n",
        },
        host: &host,
    };
    let refused = Tools
        .call(&cx, "control_chat", &json!({"tool":"chats_list"}))
        .unwrap_err();
    assert!(refused.to_string().contains("Permissions"), "{refused}");
    assert!(!own.is_empty());
}
