use super::*;

// The host preparation API is read-only. Fixtures explicitly publish its result.
fn enqueue_continuation(state: &mut State, chat: &str, from: usize, objective: String, kind: RunKind, source: &str) -> Result<String, ContinuationError> {
    let run = continuation(state, chat, from, objective, kind, source)?;
    let id = run.id.clone();
    state.runtime.runs.push(run);
    state.runtime.notice = "Run queued.".into();
    crate::worker::wake();
    Ok(id)
}

#[test]
fn preparing_a_continuation_does_not_publish_or_change_state() {
    let mut state = State::default();
    let chat = create(&mut state);
    send(&mut state, &chat, "Original task", "test").unwrap();
    state.runtime.runs[0].status = "completed".into();
    let before = serde_json::to_vec(&state).unwrap();
    let run = continuation(&state, &chat, 0, "Continue".into(), RunKind::PortDecision, "test").unwrap();
    assert_eq!(run.chat_id.as_deref(), Some(chat.as_str()));
    assert_eq!(run.status, "queued");
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
}

#[test]
fn invalid_continuation_input_leaves_the_queue_and_notice_unchanged() {
    let mut state = State::default();
    state
        .runtime
        .enqueue("original".into(), RunKind::Task, "test");
    state.runtime.runs[0].status = "completed".into();
    state.runtime.notice = "unchanged".into();
    for objective in [
        String::new(),
        "x".repeat(control_context::limits::CHAT_MESSAGE_CHARS + 1),
    ] {
        assert!(
            enqueue_continuation(
                &mut state,
                "chat",
                0,
                objective,
                RunKind::PortDecision,
                "test"
            )
            .is_err()
        );
        assert_eq!(state.runtime.runs.len(), 1);
        assert_eq!(state.runtime.notice, "unchanged");
    }
}

#[test]
#[ignore = "release continuation benchmark and perf fixture; synthetic state, no model calls"]
fn profile_continuation_admission() {
    use std::{hint::black_box, time::Instant};
    let rounds = std::env::var("CONTROL_CONTINUATION_ROUNDS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(1, 100_000);
    for retained in [16, 1024, 20_001] {
        let mut state = State::default();
        state
            .runtime
            .enqueue("original".into(), RunKind::Task, "fixture");
        let mut origin = state.runtime.runs.pop().unwrap();
        origin.status = "completed".into();
        origin.model = Some("captured-model".into());
        origin.chat_id = Some("chat".into());
        state.runtime.runs = (0..retained)
            .map(|index| {
                let mut run = origin.clone();
                run.id = format!("run-{index}");
                run.queue_order = index as u64;
                run
            })
            .collect();
        let mut samples = Vec::new();
        for batch in 0..11 {
            let start = Instant::now();
            for _ in 0..rounds {
                let id = enqueue_continuation(
                    black_box(&mut state),
                    "chat",
                    0,
                    "continue".into(),
                    RunKind::PortDecision,
                    "fixture",
                )
                .unwrap();
                black_box(id);
                black_box(state.runtime.runs.pop().unwrap());
            }
            if batch != 0 {
                samples.push(start.elapsed().as_nanos() / rounds as u128);
            }
        }
        samples.sort_unstable();
        println!(
            "{}",
            json!({"case":"continuation_admission", "retained":retained,
            "median_ns":(samples[4]+samples[5])/2, "min_ns":samples[0], "max_ns":samples[9], "rounds":rounds})
        );
    }
}
#[test]
fn long_unicode_messages_reach_runs_and_reject_overflow_without_changes() {
    let mut state = State::default();
    let id = create(&mut state);
    let text = format!(
        "{} END",
        "🦊".repeat(control_context::limits::CHAT_MESSAGE_CHARS - 4)
    );
    tool(
        &mut state,
        "control.chat.send",
        &json!({"id":id,"message":text}),
    )
    .unwrap();
    assert_eq!(state.chats[0].messages[0].text, text);
    assert_eq!(state.runtime.runs[0].objective.as_str(), text);
    for backend in [
        crate::backend::Backend::Codex,
        crate::backend::Backend::Claude,
    ] {
        state.runtime.runs[0].backend = backend;
        assert!(
            prompt(&state, &state.runtime.runs[0].id)
                .unwrap()
                .contains(&text)
        );
    }
    let before = serde_json::to_vec(&state).unwrap();
    for invalid in [
        format!("{text}x"),
        format!("/spawn {text}"),
        format!("/btw {text}"),
    ] {
        assert!(
            tool(
                &mut state,
                "control.chat.send",
                &json!({"id":id,"message":invalid,"mode":"improve"})
            )
            .is_err()
        );
        assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    }
    assert!(!render(&state, &id).unwrap().contains("maxlength='2000'"));
    assert!(!landing_mode(&state, "engineering", None, false).contains("maxlength='2000'"));
    assert_eq!(
        crate::tool_registry::lookup("control.chat.send")
            .unwrap()
            .definition["inputSchema"]["properties"]["message"]["maxLength"],
        control_context::limits::CHAT_MESSAGE_CHARS
    );
}

#[test]
fn long_command_objectives_keep_their_complete_tail() {
    let text = format!("{} final constraint", "λ".repeat(4000));
    for prefix in [
        "/spawn ",
        "/btw ",
        "/steer ",
        "/improve ",
        "!control improve ",
    ] {
        let mut state = State::default();
        let id = create(&mut state);
        send(&mut state, &id, &format!("{prefix}{text}"), "test").unwrap();
        assert_eq!(
            state.runtime.runs.last().unwrap().objective.as_str(),
            text,
            "{prefix}"
        );
    }
    assert!(
        discord::direct_command(&text).is_none(),
        "Discord retains its transport-specific bound"
    );
}
#[test]
fn conversation_index_preserves_first_matches_and_missing_run_replies() {
    let mut state = State::default();
    let id = create(&mut state);
    send(&mut state, &id, "Question", "test").unwrap();
    state.runtime.runs[0].status = "completed".into();
    state.runtime.runs[0].result = "First & original".into();
    let mut duplicate = state.runtime.runs[0].clone();
    duplicate.result = "Later duplicate".into();
    state.runtime.runs.push(duplicate);
    state.chats[0].messages.push(Message {
        text: "Missing run".into(),
        run_id: Some("missing".into()),
        reply: "Retained reply".into(),
    });
    state.chats[0].messages.push(Message {
        text: "No run".into(),
        run_id: None,
        reply: "Local reply".into(),
    });
    let conversation = crate::conversation::Conversation::of(&state, &state.chats[0]);
    assert!(
        conversation.entry(0).unwrap().run.is_some()
            && conversation.entry(1).unwrap().run.is_none()
            && conversation.entry(2).unwrap().run.is_none()
    );
    let data = detail(&state, &id).unwrap();
    assert_eq!(data["messages"][0]["reply"], "First & original");
    assert_eq!(data["messages"][1]["reply"], "Retained reply");
    assert_eq!(data["messages"][2]["reply"], "Local reply");
    let html = transcript(&state, &id).unwrap();
    assert!(
        html.contains("First &amp; original")
            && html.contains("Retained reply")
            && html.contains("Local reply")
    );
    assert!(!html.contains("Later duplicate"));
}
#[test]
fn completed_file_diffs_are_visible_escaped_and_separate_from_command_logs() {
    let mut state = State::default();
    let id = create(&mut state);
    send(&mut state, &id, "Edit the file", "test").unwrap();
    state.runtime.runs[0].status = "completed".into();
    state.runtime.runs[0].attempts.push(crate::usage::Attempt {
        activity: vec![crate::usage::Activity {
            id: "workspace-diff".into(),
            kind: "file_change".into(),
            text: "1 file · +1 −1".into(),
            output: "@@ -1 +1 @@\n-old\n+<script>\n".into(),
            status: "completed".into(),
        }],
        ..crate::usage::Attempt::default()
    });
    let html = transcript(&state, &id).unwrap();
    let diff = html
        .split("<details ")
        .find(|tag| tag.starts_with("class='edit-diff'"))
        .unwrap()
        .split('>')
        .next()
        .unwrap();
    assert!(
        diff.split_ascii_whitespace()
            .any(|attribute| attribute == "open")
    );
    assert!(html.contains("diff-line diff-add'>+&lt;script&gt;"));
    assert!(html.contains("diff-line diff-remove'>-old"));
    assert!(!html.contains("<script>"));
    assert!(!html.contains("Recent activity"));
}
#[test]
fn cancel_targets_only_this_chats_pending_work_and_refreshes_until_exit() {
    let mut state = State::default();
    let parent = create(&mut state);
    send(&mut state, &parent, "Build parser", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    send(&mut state, &parent, "Add tests", "test").unwrap();
    let other = create(&mut state);
    send(&mut state, &other, "Other task", "test").unwrap();
    let other_run = state.runtime.runs[2].id.clone();
    assert!(send(&mut state, &parent, &format!("/cancel {other_run}"), "test").is_err());
    let revision = state.chats[0].revision;
    send(&mut state, &parent, "/cancel", "test").unwrap();
    assert_eq!(state.runtime.runs[0].status, "cancelling");
    assert_eq!(state.runtime.runs[1].status, "queued");
    assert_eq!(state.runtime.runs[2].status, "queued");
    assert!(state.chats[0].revision > revision);
    let html = transcript(&state, &parent).unwrap();
    assert!(html.contains("Stopping…") && !html.contains("http-equiv='refresh'"));
    let queued = state.runtime.runs[1].id.clone();
    send(&mut state, &parent, &format!("/cancel {queued}"), "test").unwrap();
    assert_eq!(state.runtime.runs[1].status, "cancelled");
    assert!(transcript(&state, &parent).unwrap().contains("Cancelled"));
    assert_eq!(state.runtime.runs.len(), 3);
}
#[test]
fn explicit_spawn_inherits_snapshot_without_workspace_authority_or_recursion() {
    let mut state = State::default();
    let parent = create(&mut state);
    state.runtime.model = Some("parent-model".into());
    send(&mut state, &parent, "Parent task", "test").unwrap();
    state.chats[0].workspace = Some("parent-output".into());
    state.chats[0].improve_mode = true;
    state.runtime.model = Some("changed-global".into());
    send(&mut state, &parent, "/spawn /improve inspect this", "test").unwrap();
    assert_eq!(state.chats.len(), 2);
    let child = &state.chats[1];
    assert_eq!(child.parent_chat.as_deref(), Some(parent.as_str()));
    assert_eq!(child.actor, state.chats[0].actor);
    assert!(!child.improve_mode);
    assert_eq!(child.workspace.as_deref(), Some("parent-output"));
    assert!(child.improve_workspace.is_none());
    assert!(state.runtime.runs[1].attempts.is_empty());
    assert!(child.fork_context.as_ref().unwrap().contains("Parent task"));
    assert_eq!(state.runtime.runs[1].action, Action::Task);
    assert_eq!(state.runtime.runs[1].objective, "/improve inspect this");
    assert_eq!(state.runtime.runs[1].model.as_deref(), Some("parent-model"));
    assert_eq!(child.lenses, state.runtime.runs[1].lenses);
    assert!(
        transcript(&state, &parent)
            .unwrap()
            .contains("target='_top'>Agent conversation")
    );
    assert!(render(&state, &child.id).unwrap().contains("Parent chat"));
    let restored: State = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
    assert_eq!(restored.chats[1].parent_chat, child.parent_chat);
    assert_eq!(restored.chats[1].fork_context, child.fork_context);
}

#[test]
fn spawn_rejection_leaves_no_orphan_and_cannot_elevate_unknown_actor() {
    let mut state = State::default();
    let parent = create(&mut state);
    assert!(fork(&mut state, &parent, "", "test").is_err());
    state.chats[0].actor = Actor::Unknown;
    assert!(fork(&mut state, &parent, "/improve change source", "test").is_err());
    assert_eq!(state.chats.len(), 1);
    assert!(state.runtime.runs.is_empty());
    state.chats[0].actor = Actor::LocalOperator;
    for _ in 0..crate::scheduler::MAX_PENDING {
        state
            .runtime
            .enqueue_for(
                "task".into(),
                RunKind::Chat,
                "test",
                Actor::LocalOperator,
                Action::Task,
            )
            .unwrap();
    }
    assert!(fork(&mut state, &parent, "overflow", "test").is_err());
    assert_eq!(state.chats.len(), 1);
}
#[test]
fn slash_commands_replace_mode_form_controls_and_escape_chat_text() {
    let mut state = State::default();
    let id = create(&mut state);
    let landing = landing_mode(&state, "engineering", None, true);
    assert!(landing.contains(">/improve </textarea>"));
    assert!(landing.contains("role='listbox'"));
    assert!(!landing.contains("name='improve_mode'"));
    assert!(!landing.contains("mode_present"));
    assert!(!landing.contains("type='checkbox' name='improve"));
    send(
        &mut state,
        &id,
        "/improve <script>alert(1)</script>",
        "test",
    )
    .unwrap();
    let html = render(&state, &id).unwrap();
    assert!(html.contains("class='chat-command active-command'"));
    assert!(html.contains("data-command='/improve off'"));
    let transcript = transcript(&state, &id).unwrap();
    assert!(transcript.contains("<span class='chat-command'>/improve</span> &lt;script&gt;"));
    assert!(!transcript.contains("<script>"));
    assert_eq!(
        command_text("/improve off"),
        "<span class='chat-command'>/improve off</span>"
    );
    assert_eq!(
        command_text("/improvements <img>"),
        "/improvements &lt;img&gt;"
    );
    state.chats[0].actor = Actor::Unknown;
    assert!(command_menu(&state.chats[0].actor).is_empty());
    let user_menu = command_menu_for(true, false);
    for command in ["/btw", "/steer", "/spawn"] {
        assert!(user_menu.contains(&format!("data-command='{command}'")));
    }
    assert!(!user_menu.contains("/improve"));
}
#[test]
fn side_question_captures_context_without_reusing_parent_workspace_or_authority() {
    let mut state = State::default();
    let parent = create(&mut state);
    state.chats[0].workspace = Some("published-parent".into());
    state.chats[0].improve_workspace = Some("source-parent".into());
    send(&mut state, &parent, "/improve Build a parser", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    send(&mut state, &parent, "/btw What is a parser?", "test").unwrap();
    assert_eq!(state.chats.len(), 2);
    let child = &state.chats[1];
    assert_eq!(child.parent_chat.as_deref(), Some(parent.as_str()));
    assert!(
        child
            .fork_context
            .as_ref()
            .unwrap()
            .contains("Build a parser")
    );
    assert_eq!(child.workspace.as_deref(), Some("published-parent"));
    assert!(child.improve_workspace.is_none());
    assert!(!child.improve_mode);
    assert!(state.chats[0].improve_mode);
    assert_eq!(
        state.chats[0].workspace.as_deref(),
        Some("published-parent")
    );
    assert_eq!(state.runtime.runs[0].status, "running");
    assert_eq!(state.runtime.runs[1].action, Action::Task);
    assert!(
        transcript(&state, &parent)
            .unwrap()
            .contains(&format!("href='/chat/{}'", child.id))
    );
    let captured = child.fork_context.clone();
    state.runtime.runs[0].result = "Later result".into();
    assert_eq!(state.chats[1].fork_context, captured);
    assert!(send(&mut state, &parent, "/btw", "test").is_err());
    assert_eq!(state.chats.len(), 2);
}
#[test]
fn steering_is_durable_ordered_and_never_mutates_an_active_request() {
    let mut state = State::default();
    let id = create(&mut state);
    send(&mut state, &id, "Build a parser", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    let original = state.runtime.runs[0].objective.clone();
    send(&mut state, &id, "Also add examples", "test").unwrap();
    send(&mut state, &id, "/steer Keep the API small", "test").unwrap();
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(restored.runtime.runs[0].objective, original);
    assert_eq!(restored.runtime.runs[0].status, "running");
    assert_eq!(restored.runtime.runs[1].objective, "Also add examples");
    assert_eq!(restored.runtime.runs[2].objective, "Keep the API small");
    assert_eq!(restored.runtime.runs[2].kind, RunKind::Steering);
    assert!(
        restored
            .runtime
            .runs
            .windows(2)
            .all(|pair| pair[0].queue_order < pair[1].queue_order)
    );
    assert!(
        transcript(&restored, &id)
            .unwrap()
            .contains("Queued · next turn")
    );
    restored.runtime.runs[0].status = "completed".into();
    restored.runtime.runs[0].result = "Parser complete".into();
    let steering = restored.runtime.runs[2].id.clone();
    assert!(
        prompt(&restored, &steering)
            .unwrap()
            .contains("Parser complete")
    );
    assert!(send(&mut restored, &id, "/steer", "test").is_err());
    assert_eq!(restored.runtime.runs.len(), 3);
    assert_eq!(
        command_text("/steer <img>"),
        "<span class='chat-command'>/steer</span> &lt;img&gt;"
    );
}
#[test]
fn improve_mode_persists_discussion_and_can_exit_without_changing_queued_authority() {
    let mut state = State::default();
    let id = create(&mut state);
    send(&mut state, &id, "/improve", "test").unwrap();
    assert!(state.runtime.runs.is_empty());
    assert!(state.chats[0].improve_mode);
    send(&mut state, &id, "Could this design be simpler?", "test").unwrap();
    assert_eq!(state.runtime.runs[0].action, Action::Improve);
    let first = state.runtime.runs[0].id.clone();
    assert!(
        prompt(&state, &first)
            .unwrap()
            .contains("questions and design discussion")
    );
    let mut restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    send(&mut restored, &id, "Okay, implement that change", "test").unwrap();
    assert_eq!(restored.runtime.runs[1].action, Action::Improve);
    send(&mut restored, &id, "/improve off", "test").unwrap();
    assert!(!restored.chats[0].improve_mode);
    assert_eq!(restored.runtime.runs.len(), 2);
    send(&mut restored, &id, "Build a small Rust project", "test").unwrap();
    assert_eq!(restored.runtime.runs[2].action, Action::Task);
    assert_eq!(restored.runtime.runs[0].action, Action::Improve);
    restored.chats[0].actor = Actor::Unknown;
    assert!(send(&mut restored, &id, "/improve", "test").is_err());
    assert!(!restored.chats[0].improve_mode);
    restored.chats[0].improve_mode = true;
    assert!(send(&mut restored, &id, "Change Control", "test").is_err());
    assert_eq!(restored.runtime.runs.len(), 3);
}
#[test]
fn attempt_activity_and_context_trimming_survive_serialization_and_escape_html() {
    let mut state = State::default();
    let id = create(&mut state);
    state.chats[0].messages.push(Message {
        text: "x".repeat(21000),
        run_id: None,
        reply: String::new(),
    });
    send(&mut state, &id, "Check the build", "test").unwrap();
    let run_id = state.runtime.runs[0].id.clone();
    assert_eq!(omitted_history(&state, &run_id), 1);
    state.runtime.runs[0].attempts.push(crate::usage::Attempt {
        omitted_history: 1,
        activity: vec![crate::usage::Activity {
            id: "cmd1".into(),
            kind: "command_execution".into(),
            text: "echo <script>".into(),
            output: "<img src=x onerror=bad()>".into(),
            status: "completed".into(),
        }],
        ..crate::usage::Attempt::default()
    });
    state.runtime.runs[0].status = "failed".into();
    let mut stored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    stored.runtime.action(
        "/retry-run",
        &std::collections::HashMap::from([("id".into(), run_id)]),
    );
    assert_eq!(stored.runtime.runs[0].attempts.len(), 1);
    assert_eq!(stored.runtime.runs[0].status, "queued");
    let html = transcript(&stored, &id).unwrap();
    assert!(html.contains("1 earlier exchanges excluded"));
    assert!(html.contains("echo &lt;script&gt;"));
    assert!(html.contains("&lt;img src=x onerror=bad()&gt;"));
    assert!(!html.contains("<script>"));
    assert_eq!(stored.chats[0].messages[0].text.len(), 21000);
    assert!(
        !prompt(&stored, &stored.runtime.runs[0].id)
            .unwrap()
            .contains(&"x".repeat(21000))
    );
}
#[test]
fn runs_pin_nested_definitions_and_document_contents() {
    let mut state = State::default();
    state.documents.push(crate::knowledge::Document {
        id: "doc-1".into(),
        title: "Architecture.md".into(),
        body: "Original document body".into(),
        revision: 1,
        saved_at: None,
    });
    let mut definition = state
        .lens_library
        .get("architecture-notes")
        .unwrap()
        .clone();
    definition.revision = 2;
    definition.documents.push("doc-1".into());
    state.lens_library.definitions.push(definition.clone());
    let chat = tool(
        &mut state,
        "control.chat.create",
        &json!({"lenses":["engineering"]}),
    )
    .unwrap();
    let id = chat["id"].as_str().unwrap();
    send(&mut state, id, "Check the architecture", "test").unwrap();
    let first = state.runtime.runs[0].id.clone();
    state.documents[0].body = "Changed document body".into();
    state.documents[0].revision = 2;
    definition.revision = 3;
    definition.instructions = "New guidance".into();
    state.lens_library.definitions.push(definition);
    send(&mut state, id, "Check it again", "test").unwrap();
    let old = prompt(&state, &first).unwrap();
    assert!(old.contains("Original document body"));
    assert!(!old.contains("Changed document body"));
    assert!(!old.contains("New guidance"));
    assert_eq!(
        state.runtime.runs[0].context.as_ref().unwrap().documents[0].revision,
        1
    );
    let new = prompt(&state, &state.runtime.runs[1].id).unwrap();
    assert!(new.contains("Changed document body"));
    assert!(new.contains("New guidance"));
    let stored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(
        stored.runtime.runs[0].context,
        state.runtime.runs[0].context
    );
}
#[test]
fn preview_library_and_chat() {
    let Ok(directory) = std::env::var("CONTROL_LIBRARY_PREVIEW") else {
        return;
    };
    std::fs::create_dir_all(&directory).unwrap();
    let mut state = State::default();
    let id = create(&mut state);
    state.chats[0].lenses = Some(vec!["engineering".into(), "evidence".into()]);
    state.chats[0].room = Some("engineering".into());
    state.chats[0].title = "Architecture discussion".into();
    state.chats[0].messages.push(Message{text:"How should the context fit together?".into(),run_id:None,reply:"Engineering composes repository tools, evidence guidance, and architecture notes. Each run keeps its resolved context.".into()});
    state.collective.posts.push(crate::rooms::Post { room: "engineering".into(), text: "The layout now keeps room navigation on the left and linked chats on the right. Shared updates use the same theme as the chat window.".into(), chat: Some(id.clone()), run: None, key: String::new(), delivery: Default::default() });
    let inline = |html: String| {
        let mut html = html;
        for (name, css) in [
            ("style.css", include_str!("../../assets/style.css")),
            ("runtime.css", include_str!("../../assets/runtime.css")),
            (
                "collective.css",
                include_str!("../../assets/collective.css"),
            ),
            ("workspace.css", include_str!("../../assets/workspace.css")),
            (
                "navigation.css",
                include_str!("../../assets/navigation.css"),
            ),
        ] {
            html = html
                .replace(
                    &format!("<link rel='stylesheet' href='/{name}'>"),
                    &format!("<style>{css}</style>"),
                )
                .replace(
                    &format!("<link rel=\"stylesheet\" href=\"/{name}\">"),
                    &format!("<style>{css}</style>"),
                );
        }
        html
    };
    let session = crate::render(&state, "session", "none", &id)
        .replace("<body>", "<body class='session-view'>")
        .replace(
            &format!("src='/chat/{id}/transcript#latest'"),
            &format!(
                "srcdoc='{}'",
                escape(&inline(transcript(&state, &id).unwrap()))
            ),
        );
    let landing = crate::render(
        &state,
        "session-list",
        "none",
        &landing(&state, "none", None),
    )
    .replace("<body>", "<body class='session-view'>");
    for (file, html) in [
        (
            "rooms.html",
            crate::render(&state, "rooms", "none", "engineering"),
        ),
        ("chat.html", session),
        ("new.html", landing),
        ("transcript.html", transcript(&state, &id).unwrap()),
        (
            "lenses.html",
            crate::render(&state, "lenses", "engineering", "general"),
        ),
    ] {
        std::fs::write(std::path::Path::new(&directory).join(file), inline(html)).unwrap();
    }
}
#[test]
fn composing_and_changing_lenses_preserves_each_runs_selection() {
    let mut state = State::default();
    let chat = tool(
        &mut state,
        "control.chat.create",
        &json!({"lenses":["research","engineering","research"],"design_context":"knowledge"}),
    )
    .unwrap();
    let id = chat["id"].as_str().unwrap();
    assert_eq!(chat["lenses"], json!(["engineering", "research"]));
    send(&mut state, id, "Review this design", "test").unwrap();
    let first = state.runtime.runs[0].id.clone();
    tool(
        &mut state,
        "control.chat.lenses",
        &json!({"id":id,"lenses":[]}),
    )
    .unwrap();
    send(&mut state, id, "Continue without lenses", "test").unwrap();
    assert!(
        prompt(&state, &first)
            .unwrap()
            .contains("Engineering + Research")
    );
    assert!(
        !prompt(&state, &state.runtime.runs[1].id)
            .unwrap()
            .contains("Selected context lenses:")
    );
    assert_eq!(state.runtime.runs[0].action, Action::Task);
    assert!(
        prompt(&state, &first)
            .unwrap()
            .contains("Design discussion: the Knowledge")
    );
    assert!(
        tool(
            &mut state,
            "control.chat.lenses",
            &json!({"id":id,"lenses":["unknown"]})
        )
        .is_err()
    );
    assert!(selected_lenses(&state.chats[0]).is_empty());
    let count = state.chats.len();
    let runs = state.runtime.runs.len();
    let page = landing(&state, "none", Some("knowledge"));
    assert!(page.contains("design of Knowledge"));
    assert_eq!(state.chats.len(), count);
    assert_eq!(state.runtime.runs.len(), runs);
    assert!(
        tool(
            &mut state,
            "control.chat.create",
            &json!({"design_context":"<script>"})
        )
        .is_err()
    );
}
#[test]
fn optional_guidance_survives_child_wake_and_library_edits_without_retroactive_changes() {
    let mut state = State::default();
    let created = tool(
        &mut state,
        "control.chat.create",
        &json!({"lenses":["autonomy","investigation","coordination"]}),
    )
    .unwrap();
    let id = created["id"].as_str().unwrap().to_owned();
    send(&mut state, &id, "Investigate this blocked task", "test").unwrap();
    let parent = state.runtime.runs[0].id.clone();
    fork(&mut state, &id, "Inspect an independent hypothesis", "test").unwrap();
    let child = state.runtime.runs[1].id.clone();
    let mut revised = state.lens_library.get("investigation").unwrap().clone();
    revised.revision += 1;
    revised.instructions = "Future investigation guidance only".into();
    state.lens_library.definitions.push(revised);
    let mut revised = state.lens_library.get("coordination").unwrap().clone();
    revised.revision += 1;
    revised.instructions = "Future coordination guidance only".into();
    state.lens_library.definitions.push(revised);
    let wake = enqueue_continuation(
        &mut state,
        &id,
        0,
        "Inspect a changed observation".into(),
        RunKind::BotWake,
        "test",
    )
    .unwrap();
    state
        .chats
        .iter_mut()
        .find(|chat| chat.id == id)
        .unwrap()
        .messages
        .push(Message {
            text: "Inspect a changed observation".into(),
            run_id: Some(wake.clone()),
            reply: String::new(),
        });
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    for run in [&parent, &child, &wake] {
        let text = prompt(&restored, run).unwrap();
        assert!(text.contains("investigation@1") && text.contains("three distinct probes"));
        assert!(text.contains("coordination@2"));
        assert_eq!(text.matches("three distinct probes").count(), 1);
        assert!(!text.contains("Future investigation guidance only"));
        assert!(!text.contains("Future coordination guidance only"));
    }
    send(&mut state, &id, "Start a new authorized step", "test").unwrap();
    let next = prompt(&state, &state.runtime.runs.last().unwrap().id).unwrap();
    assert!(
        next.contains("investigation@2") && next.contains("Future investigation guidance only")
    );
    assert!(next.contains("coordination@3") && next.contains("Future coordination guidance only"));
    tool(
        &mut state,
        "control.chat.lenses",
        &json!({"id":id,"lenses":[]}),
    )
    .unwrap();
    send(
        &mut state,
        &id,
        "Continue without optional guidance",
        "test",
    )
    .unwrap();
    let plain = prompt(&state, &state.runtime.runs.last().unwrap().id).unwrap();
    assert!(!plain.contains("Captured lens guidance") && !plain.contains("three distinct probes"));
    assert!(!plain.contains("Future coordination guidance only"));
}
#[test]
fn chat_context_is_captured_and_does_not_follow_browser_defaults() {
    let mut state = State::default();
    let created = tool(
        &mut state,
        "control.chat.create",
        &json!({"lens":"research"}),
    )
    .unwrap();
    let id = created["id"].as_str().unwrap();
    send(&mut state, id, "Find a useful source", "test").unwrap();
    let page = crate::render(&state, "session", "engineering", id);
    assert!(page.contains("value='research' form='session-message' checked"));
    assert_eq!(detail(&state, id).unwrap()["lens"], "research");
    assert!(
        prompt(&state, &state.runtime.runs[0].id)
            .unwrap()
            .contains("Selected context lenses: Research")
    );
    let count = state.chats.len();
    assert!(
        tool(
            &mut state,
            "control.chat.create",
            &json!({"lens":"unknown"})
        )
        .is_err()
    );
    assert_eq!(state.chats.len(), count);
    let legacy: Session = serde_json::from_value(
        json!({"id":"old","actor":{"type":"local_operator"},"title":"Old chat","messages":[]}),
    )
    .unwrap();
    assert!(legacy.lens.is_none());
}
#[test]
fn chats_keep_context_separate_and_keep_original_identity() {
    let mut state = State::default();
    let first = create(&mut state);
    let second = create(&mut state);
    send(&mut state, &first, "Remember the cedar tree", "test").unwrap();
    state.runtime.runs[0].status = "completed".into();
    state.runtime.runs[0].result = "Cedar recorded".into();
    send(&mut state, &second, "Other chat", "test").unwrap();
    send(&mut state, &first, "What tree?", "test").unwrap();
    let context = prompt(&state, &state.runtime.runs[2].id).unwrap();
    assert!(context.contains("Cedar recorded"));
    assert!(!context.contains("Other chat"));
    state.chats[0].actor = Actor::Unknown;
    assert!(send(&mut state, &first, "/improve change authority", "test").is_err());
    assert!(detail(&state, &first).is_err());
}
#[test]
fn improvement_action_and_session_revision_are_preserved() {
    let mut state = State::default();
    let id = create(&mut state);
    send(&mut state, &id, "/improve refine the UI", "test").unwrap();
    assert_eq!(state.runtime.runs[0].action, Action::Improve);
    assert_eq!(detail(&state, &id).unwrap()["revision"], 1);
    assert!(
        !transcript(&state, &id)
            .unwrap()
            .contains("http-equiv='refresh'")
    );
}
#[test]
fn automatic_updates_are_not_presented_as_user_messages() {
    let mut state = State::default();
    let id = create(&mut state);
    send(&mut state, &id, "Delegated task · completed", "test").unwrap();
    state.runtime.runs[0].kind = RunKind::DelegationResult;
    let html = transcript(&state, &id).unwrap();
    assert!(html.contains("aria-label='Control update'>Delegated task · completed"));
    assert!(!html.contains("aria-label='You'"));
    assert!(
        crate::conversation::Conversation::of(&state, &state.chats[0])
            .entry(0)
            .unwrap()
            .render()
            .starts_with("Control event:")
    );
    state.runtime.runs[0].kind = RunKind::RoomNotification;
    assert!(
        !transcript(&state, &id)
            .unwrap()
            .contains("aria-label='You'")
    );
    state.runtime.runs[0].kind = RunKind::DesignAnswer;
    assert!(
        transcript(&state, &id)
            .unwrap()
            .contains("aria-label='You'")
    );
    assert!(
        crate::conversation::Conversation::of(&state, &state.chats[0])
            .entry(0)
            .unwrap()
            .render()
            .starts_with("User:")
    );
}
#[test]
fn html_escapes_chat_and_api_does_not_accept_actor_overrides() {
    let mut state = State::default();
    let id = create(&mut state);
    tool(
        &mut state,
        "control.chat.send",
        &json!({"id":id,"message":"<script>bad</script>","actor":{"type":"unknown"}}),
    )
    .unwrap();
    assert!(transcript(&state, &id).unwrap().contains("&lt;script&gt;"));
    assert_eq!(state.runtime.runs[0].actor, Actor::LocalOperator);
    assert!(!valid_id("../../.env"));
}
#[test]
fn existing_chats_attach_to_rooms_unless_a_bot_or_listener_owns_the_inbox() {
    let mut state = State::default();
    let id = create(&mut state);
    send(&mut state, &id, "Watch the farm", "test").unwrap();
    state.runtime.runs[0].status = "completed".into();
    assert!(send(&mut state, &id, "/room lobby", "test").is_err());
    send(&mut state, &id, "/room general", "test").unwrap();
    assert_eq!(
        state.chats[0].room.as_deref(),
        Some(format!("general/{id}").as_str())
    );
    assert!(
        state.chats[0]
            .messages
            .last()
            .unwrap()
            .reply
            .contains("#general")
    );
    send(&mut state, &id, "/listen", "test").unwrap();
    assert!(
        send(&mut state, &id, "/room research", "test").is_err(),
        "a listener owns the inbox"
    );
    send(&mut state, &id, "/listen off", "test").unwrap();
    send(&mut state, &id, "/bot on", "test").unwrap();
    assert!(
        send(&mut state, &id, "/room off", "test").is_err(),
        "a bot owns the inbox"
    );
    send(&mut state, &id, "/bot off", "test").unwrap();
    assert!(!crate::bots::defined(&state, &id));
    crate::rooms::ensure_inbox(&mut state, &id).unwrap();
    assert!(
        state
            .collective
            .room_watches
            .iter()
            .any(|watch| watch.chat == id)
    );
    send(&mut state, &id, "/room off", "test").unwrap();
    assert_eq!(state.chats[0].room, None);
    assert!(
        !state
            .collective
            .room_watches
            .iter()
            .any(|watch| watch.chat == id),
        "inbox watches of the old room are dropped"
    );
}

fn tree() -> (State, String, String, String, String) {
    let mut state = State::default();
    let lead = create(&mut state);
    send(&mut state, &lead, "Lead the playtest", "test").unwrap();
    state.runtime.runs[0].status = "completed".into();
    let spawn = |state: &mut State, parent: &str, status: &str| {
        let chat = create(state);
        state
            .chats
            .iter_mut()
            .find(|s| s.id == chat)
            .unwrap()
            .parent_chat = Some(parent.into());
        send(state, &chat, "Work", "test").unwrap();
        state.runtime.runs.last_mut().unwrap().status = status.into();
        chat
    };
    let child = spawn(&mut state, &lead, "completed");
    let grandchild = spawn(&mut state, &child, "running");
    let unrelated = create(&mut state);
    send(&mut state, &unrelated, "Other work", "test").unwrap();
    state.runtime.runs.last_mut().unwrap().status = "running".into();
    crate::bots::command(&mut state, &child, "on every 5").unwrap();
    (state, lead, child, grandchild, unrelated)
}
fn status<'a>(state: &'a State, chat: &str) -> &'a str {
    state
        .runtime
        .runs
        .iter()
        .rev()
        .find(|run| run.chat_id.as_deref() == Some(chat))
        .unwrap()
        .status
        .as_str()
}

#[test]
fn cancel_all_reaches_descendants_from_an_idle_chat() {
    let (mut state, lead, child, grandchild, unrelated) = tree();
    assert!(
        send(&mut state, &lead, "/cancel", "test").is_err(),
        "plain /cancel only targets this chat"
    );
    send(&mut state, &lead, "/cancel all", "test").unwrap();
    assert_eq!(status(&state, &grandchild), "cancelling");
    assert_eq!(status(&state, &unrelated), "running");
    assert!(
        !crate::bots::defined(&state, &child),
        "an idle bot stops at once"
    );
    let reply = &state
        .chats
        .iter()
        .find(|s| s.id == lead)
        .unwrap()
        .messages
        .last()
        .unwrap()
        .reply;
    assert!(
        reply.contains("Cancelled 1 run(s) and stopped 1 bot(s) across 3 chat(s)"),
        "{reply}"
    );
}

#[test]
fn cancel_all_rolls_back_descendants_when_the_save_fails() {
    let (mut state, lead, _, _, _) = tree();
    let before = serde_json::to_vec(&state).unwrap();
    let checkpoint = Checkpoint::capture(&state, &std::iter::once(lead.as_str()).collect());
    send(&mut state, &lead, "/cancel all", "test").unwrap();
    checkpoint.restore(&mut state);
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
}
