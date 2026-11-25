use super::*;
use control::permissions::Approvals;

/// An operator DM (`root`) whose agent runs in `own`, with an unrelated DM.
fn task() -> (Store, String, String, String) {
    let mut state = State::default();
    let root = crate::chat::create(&mut state);
    let own = crate::chat::create(&mut state);
    state.chats.last_mut().unwrap().parent_chat = Some(root.clone());
    let other = crate::chat::create(&mut state);
    (crate::store::Store::new(state), root, own, other)
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
            .guard()
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
    assert!(shared.guard().unwrap().chats.iter().all(|c| c.project.is_none()));
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
        now: crate::design::UnixSeconds::now(),
    };
    let refused = Tools
        .call(&cx, "control_chat", &json!({"tool":"chats_list"}))
        .unwrap_err();
    assert!(refused.to_string().contains("Permissions"), "{refused}");
    assert!(!own.is_empty());
}

/// What an agent is told of its sends: the message's number and status,
/// and a command's reply. Pinned before sends stopped building the chat's
/// whole detail for it.
#[test]
fn agents_are_told_the_number_status_and_reply_of_their_sends() {
    let (shared, _, own, _) = task();
    let mut scoped = Scoped {
        shared: &shared,
        own: own.clone(),
    };
    let mut tool = |name: &str, arguments: Value| call(&mut scoped, name, &arguments, Approvals::default());
    let created = tool("chat_create", json!({"title":"Sender"})).unwrap();
    let child = created
        .strip_prefix("Created chat ")
        .unwrap()
        .trim_end_matches('.')
        .to_owned();
    let told = [
        tool("chat_send", json!({"id":child,"message":"First"})),
        tool("chat_send", json!({"id":child,"message":"Second"})),
        tool("chat_cancel", json!({"id":child})),
        tool("chat_cancel", json!({"id":child,"all":true})),
        tool("chat_send", json!({"id":child,"message":"After the cancel"})),
        tool("chat_send", json!({"id":own,"message":"Own chat"})),
        tool("chat_send", json!({"id":"c-1-1","message":"Nobody"})),
    ]
    .map(|told| told.map_or_else(|error| format!("error: {error}"), |told| told.replace(&child, "CHILD")));
    assert_eq!(
        told,
        [
            "Sent to CHILD as message #0 [queued]. Follow it with chat_read.",
            "Sent to CHILD as message #1 [queued]. Follow it with chat_read.",
            "Pending run cancelled.",
            "Cancelled 1 run(s) and stopped 0 bot(s) across 1 chat(s). Services keep running; /services lists them and /services stop all stops them.",
            "Sent to CHILD as message #4 [queued]. Follow it with chat_read.",
            "error: Agents change the DMs they spawned, not their own chat.",
            "error: Chat not found.",
        ]
    );
}

/// An agent's sends to a DM below it whose actor may not run a task, and to
/// a chat that does not exist, are refused as before and change nothing.
#[test]
fn agents_sends_without_task_authority_are_refused_as_before() {
    let (shared, _, own, _) = task();
    let mut scoped = Scoped {
        shared: &shared,
        own: own.clone(),
    };
    let created = call(
        &mut scoped,
        "chat_create",
        &json!({"title":"Refused"}),
        Approvals::default(),
    )
    .unwrap();
    let child = created
        .strip_prefix("Created chat ")
        .unwrap()
        .trim_end_matches('.')
        .to_owned();
    let actors = [
        crate::identity::Actor::Unknown,
        crate::identity::Actor::Discord {
            user_id: "5".parse().unwrap(),
            channel_id: "6".parse().unwrap(),
        },
        crate::chat::discord_actor("d-7-8").unwrap(),
    ];
    let mut told = Vec::new();
    for actor in actors {
        shared
            .write(|state| state.chats.iter_mut().find(|c| c.id == child).unwrap().actor = actor)
            .unwrap();
        let before = shared.read(|state| serde_json::to_vec(state).unwrap()).unwrap();
        for (name, arguments) in [
            ("chat_send", json!({"id":child,"message":"Not allowed"})),
            ("chat_model", json!({"id":child})),
            ("chat_model", json!({"id":child,"model":"codex"})),
            ("chat_cancel", json!({"id":child})),
            ("chat_cancel", json!({"id":child,"all":true})),
        ] {
            told.push(
                call(&mut scoped, name, &arguments, Approvals::default())
                    .map_or_else(|error| format!("error: {error}"), |told| told.replace(&child, "CHILD")),
            );
        }
        assert!(shared.read(|state| serde_json::to_vec(state).unwrap()).unwrap() == before);
    }
    for name in ["chat_model", "chat_cancel"] {
        told.push(
            call(&mut scoped, name, &json!({"id":"c-1-1"}), Approvals::default())
                .unwrap_or_else(|error| format!("error: {error}")),
        );
    }
    // No policy file on a test node: every actor but the local operator is refused.
    let policy = "error: Discord access policy must be a private regular file (mode 0600, at most 64 KiB)";
    let actor = ["error: Chat not found.", policy, policy, policy, policy];
    let missing = "error: Only chats in your own task (your chat and the chats spawned below it).";
    assert_eq!(
        told,
        [actor, actor, actor]
            .concat()
            .into_iter()
            .chain([missing, missing])
            .collect::<Vec<_>>()
    );
}
