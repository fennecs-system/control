
use super::*;
use crate::identity::{Action, Actor};
use crate::runtime::RunKind;

#[test]
fn slots_overlap_chats_but_never_reorder_or_overlap_a_conversation() {
    let mut runtime = Runtime::default();
    runtime.execution.max_parallel_runs = 2;
    for chat in ["a", "a", "b", "c"] {
        runtime.enqueue("Task".into(), RunKind::Chat, "test");
        runtime.runs.last_mut().unwrap().chat_id = Some(chat.into());
    }
    assert_eq!(next(&runtime), Some(0));
    runtime.runs[0].status = "running".into();
    assert_eq!(next(&runtime), Some(2));
    runtime.runs[2].status = "cancelling".into();
    assert!(next(&runtime).is_none());
    runtime.runs[0].status = "completed".into();
    assert_eq!(next(&runtime), Some(1));
    runtime.runs[0].status = "waiting".into();
    runtime.runs[0].question = Some(crate::design::Question {
        id: "q".into(),
        nonce: "n".into(),
        generation: "g".into(),
        input: crate::design::Input {
            question: "Pick".into(),
            options: vec!["a".into(), "b".into()],
            recommended: None,
        },
        answer: None,
        continuation: None,
    });
    assert_eq!(
        next(&runtime),
        Some(3),
        "Paused questions reserve their chat, not a model slot"
    );
    runtime.runs[0].question = None;
    runtime.runs[0].delegation = Some(crate::delegation::Delegation {
        input: crate::delegation::Input {
            objective: "Inspect".into(),
            role: crate::delegation::Role::Worker,
        },
        nonce: "n".into(),
        generation: "g".into(),
        backend: crate::backend::Backend::Codex,
        model: "test".into(),
        child: Some("b".into()),
        continuation: None,
    });
    assert_eq!(
        next(&runtime),
        Some(3),
        "Delegation reserves its chat but releases its slot"
    );
    runtime.paused = Some("Uncertain resource ownership".into());
    assert!(next(&runtime).is_none());
}
#[test]
fn private_limits_are_validated_and_do_not_come_from_saved_runtime_state() {
    use std::os::unix::fs::PermissionsExt;
    let directory = std::env::temp_dir().join(format!(
        "control-limits-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let file = directory.join("execution.json");
    assert_eq!(load_limits_in(&file, None).unwrap().max_parallel_runs, 3);
    assert_eq!(load_limits_in(&file, None).unwrap().max_agents_per_root, 64);
    assert_eq!(
        load_limits_in(&file, None)
            .unwrap()
            .max_pending_agent_intents,
        256
    );
    std::fs::write(&file, b"{\"max_parallel_runs\":4}").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(load_limits_in(&file, None).unwrap().max_parallel_runs, 4);
    assert_eq!(
        load_limits_in(&file, Some("1".into()))
            .unwrap()
            .max_parallel_runs,
        1
    );
    for invalid in ["0", "9", "wrong"] {
        assert!(load_limits_in(&file, Some(invalid.into())).is_err());
    }
    for invalid in [
        b"{\"max_agents_per_root\":0}".as_slice(),
        b"{\"max_pending_agent_intents\":0}".as_slice(),
        b"{\"max_agents_per_root\":65537}".as_slice(),
        b"{\"max_pending_agent_intents\":\"infinity\"}".as_slice(),
    ] {
        std::fs::write(&file, invalid).unwrap();
        assert!(load_limits_in(&file, None).is_err());
    }
    std::fs::write(
        &file,
        b"{\"max_parallel_runs\":2,\"max_agents_per_root\":100,\"max_pending_agent_intents\":500}",
    )
    .unwrap();
    assert_eq!(
        load_limits_in(&file, None).unwrap().max_agents_per_root,
        100
    );
    std::fs::write(&file, b"{\"unlimited\":true}").unwrap();
    assert!(load_limits_in(&file, None).is_err());
    std::fs::write(&file, b"{}").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(load_limits_in(&file, None).is_err());
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink("missing", &file).unwrap();
    assert!(load_limits_in(&file, None).is_err());
    let restored: Runtime =
        serde_json::from_value(serde_json::json!({"execution":{"max_parallel_runs":1000}}))
            .unwrap();
    assert_eq!(restored.execution.max_parallel_runs, 3);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn admission_is_bounded_and_dispatch_is_serial_fifo() {
    let mut runtime = Runtime::default();
    runtime.execution.max_parallel_runs = 1;
    for n in 0..MAX_PENDING {
        runtime
            .enqueue_for(
                format!("Task {n}"),
                RunKind::Chat,
                "test",
                Actor::LocalOperator,
                Action::Task,
            )
            .unwrap();
    }
    assert!(
        runtime
            .enqueue_for(
                "Overflow".into(),
                RunKind::Chat,
                "test",
                Actor::LocalOperator,
                Action::Task
            )
            .is_err()
    );
    assert_eq!(runtime.runs.len(), MAX_PENDING);
    assert_eq!(next(&runtime), Some(0));
    runtime.runs[0].status = "running".into();
    assert_eq!(next(&runtime), None);
    runtime.runs[0].status = "cancelling".into();
    assert_eq!(next(&runtime), None);
    runtime.runs[0].status = "completed".into();
    assert_eq!(next(&runtime), Some(1));
    assert!(admit(&runtime).is_ok());
}
#[test]
fn background_agents_leave_a_slot_for_conversations_and_queue_behind_them() {
    let persona = |parent: Option<&str>| {
        Some(crate::delegation::Persona {
            role: crate::delegation::Role::Worker,
            root: "main".into(),
            depth: u8::from(parent.is_some()),
            parent: parent.map(Into::into),
        })
    };
    let enqueue = |runtime: &mut Runtime, chat: &str, agent: bool| {
        runtime.enqueue("Task".into(), RunKind::Chat, "test");
        let run = runtime.runs.last_mut().unwrap();
        run.chat_id = Some(chat.into());
        if agent {
            run.persona = persona(Some("main"));
        }
    };
    let mut runtime = Runtime::default();
    runtime.execution.max_parallel_runs = 3;
    for chat in ["alice", "cass", "scout"] {
        enqueue(&mut runtime, chat, true);
    }
    enqueue(&mut runtime, "main", false);
    assert_eq!(
        next(&runtime),
        Some(3),
        "a conversation turn starts before older queued agents"
    );
    runtime.runs[3].status = "running".into();
    assert_eq!(next(&runtime), Some(0));
    runtime.runs[0].status = "running".into();
    assert_eq!(next(&runtime), Some(1));
    runtime.runs[1].status = "running".into();
    assert_eq!(next(&runtime), None);
    runtime.runs[3].status = "completed".into();
    assert_eq!(next(&runtime), None, "agents use at most all but one slot");
    enqueue(&mut runtime, "main", false);
    runtime.runs[4].persona = persona(None);
    assert_eq!(
        next(&runtime),
        Some(4),
        "root-chat continuations use the conversation lane"
    );
    runtime.runs[4].status = "running".into();
    runtime.runs[0].status = "completed".into();
    assert_eq!(
        next(&runtime),
        Some(2),
        "agents may fill all but one slot while a conversation runs"
    );
    runtime.runs[2].status = "running".into();
    runtime.runs[4].status = "completed".into();
    assert_eq!(next(&runtime), None);
    runtime.paused = Some(Default::default());
    assert_eq!(next(&runtime), None);

    let mut serial = Runtime::default();
    serial.execution.max_parallel_runs = 1;
    enqueue(&mut serial, "alice", true);
    enqueue(&mut serial, "main", false);
    assert_eq!(
        next(&serial),
        Some(1),
        "a single slot still goes to the conversation first"
    );
    serial.runs[1].status = "completed".into();
    assert_eq!(
        next(&serial),
        Some(0),
        "a single slot can still run background agents"
    );
}
#[test]
fn lane_priority_never_reorders_turns_within_a_chat() {
    let enqueue = |runtime: &mut Runtime, chat: &str, agent: bool| {
        runtime.enqueue("Task".into(), RunKind::Chat, "test");
        let run = runtime.runs.last_mut().unwrap();
        run.chat_id = Some(chat.into());
        if agent {
            run.persona = Some(crate::delegation::Persona {
                role: crate::delegation::Role::Worker,
                root: "main".into(),
                depth: 1,
                parent: Some("main".into()),
            });
        }
    };
    let mut runtime = Runtime::default();
    runtime.execution.max_parallel_runs = 3;
    enqueue(&mut runtime, "alice", true);
    enqueue(&mut runtime, "alice", false);
    enqueue(&mut runtime, "main", false);
    assert_eq!(
        next(&runtime),
        Some(2),
        "only each chat's oldest queued turn is a candidate"
    );
    runtime.runs[2].status = "running".into();
    assert_eq!(
        next(&runtime),
        Some(0),
        "a queued agent turn runs before a later message in its chat"
    );
    runtime.runs[0].status = "running".into();
    assert_eq!(next(&runtime), None);
    runtime.runs[0].status = "completed".into();
    assert_eq!(next(&runtime), Some(1));

    let mut tied = Runtime::default();
    tied.execution.max_parallel_runs = 3;
    enqueue(&mut tied, "bob", true);
    enqueue(&mut tied, "bob", false);
    for run in &mut tied.runs {
        run.queue_order = 7;
    }
    assert_eq!(
        next(&tied),
        Some(0),
        "equal queue order falls back to ledger order within a chat"
    );
}
#[test]
fn automatic_bot_turns_of_root_chats_leave_a_slot_for_people() {
    let mut runtime = Runtime::default();
    runtime.execution.max_parallel_runs = 2;
    for chat in ["farm", "mine"] {
        runtime.enqueue("Woke".into(), RunKind::BotWake, "bot");
        runtime.runs.last_mut().unwrap().chat_id = Some(chat.into());
    }
    assert_eq!(next(&runtime), Some(0));
    runtime.runs[0].status = "running".into();
    assert_eq!(
        next(&runtime),
        None,
        "a second bot may not take the last slot"
    );
    runtime.enqueue("Hello".into(), RunKind::Chat, "web");
    runtime.runs.last_mut().unwrap().chat_id = Some("main".into());
    assert_eq!(next(&runtime), Some(2));
}
