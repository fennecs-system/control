use super::*;
use crate::runtime::RunStatus;

fn request(objective: &str) -> Submission {
    Submission {
        objective: objective.into(),
        kind: RunKind::PortDecision,
        source: "approval".into(),
        actor: Actor::LocalOperator,
        action: Action::Task,
    }
}

#[test]
fn continuations_keep_captured_settings_but_start_with_fresh_execution_state() {
    for model in [None, Some("captured-model".to_owned())] {
        let mut origin = request("original").into_run(
            "origin".into(),
            4,
            crate::providers::Target {
                backend: crate::backend::Backend::Claude,
                model: model.clone(),
                ..Default::default()
            },
        );
        origin.status = RunStatus::Completed;
        origin.result = "old result".to_owned().into();
        origin.attempts.push(Default::default());
        origin.room_wake_used = true;
        origin.context = Some(control_context::Resolved {
            instructions: vec!["captured guidance".into()],
            ..Default::default()
        })
        .into();
        origin.lenses = Some(vec!["investigation".into()]);
        origin.persona = Some(crate::delegation::Persona {
            role: crate::delegation::Role::Worker,
            root: "root".into(),
            depth: 1,
            parent: Some("parent".into()),
            bundle: None,
        });
        let stale: crate::services::Grant = serde_json::from_value(serde_json::json!({
            "id":"service", "owner":{"node":"node", "chat":"chat"}, "generation":1,
        }))
        .unwrap();
        let current: crate::services::Grant = serde_json::from_value(serde_json::json!({
            "id":"service", "owner":{"node":"node", "chat":"chat"}, "generation":2,
        }))
        .unwrap();
        origin.resource_grants = vec![stale];
        let run = request("continue").into_continuation(&origin, "chat", "next".into(), 9, vec![current.clone()]);
        assert_eq!(run.target.model, model);
        assert_eq!(run.target.backend, origin.target.backend);
        assert_eq!(*run.context, *origin.context);
        assert_eq!(run.lenses, origin.lenses);
        assert_eq!(run.persona, origin.persona);
        assert_eq!(run.resource_grants, vec![current]);
        assert_eq!(run.id, "next");
        assert_eq!(run.queue_order, 9);
        assert_eq!(run.chat_id.as_deref(), Some("chat"));
        assert_eq!(&*run.objective, "continue");
        assert_eq!(run.kind, RunKind::PortDecision);
        assert_eq!(run.source, "approval");
        assert_eq!(run.actor, Actor::LocalOperator);
        assert_eq!(run.action, Action::Task);
        assert_eq!(run.status, RunStatus::Queued);
        assert!(run.attempts.is_empty() && run.result.is_empty() && run.spawns.is_empty());
        assert!(run.question.is_none() && run.continuation.is_none() && run.delegation.is_none());
        assert!(run.bot_turn.is_none() && run.emitted.is_none() && !run.room_wake_used);
    }
}

#[test]
fn continuations_establish_lineage_when_the_origin_has_no_persona() {
    let origin = request("original").into_run("origin".into(), 1, Default::default());
    let run = request("continue").into_continuation(&origin, "chat", "next".into(), 2, Vec::new());
    let persona = run.persona.clone().unwrap();
    assert_eq!(persona.root, "origin");
    assert_eq!(persona.depth, 0);
    assert_eq!(persona.parent, None);
    assert_eq!(persona.role, crate::delegation::Role::Worker);
}

/// Every status as saved by builds before the enum, in variant order.
const SAVED: [&str; 10] = [
    "queued",
    "running",
    "cancelling",
    "waiting",
    "completed",
    "blocked",
    "failed",
    "interrupted",
    "cancelled",
    "continued",
];

#[test]
fn run_statuses_save_as_the_strings_older_builds_wrote() {
    let statuses = [
        RunStatus::Queued,
        RunStatus::Running,
        RunStatus::Cancelling,
        RunStatus::Waiting,
        RunStatus::Completed,
        RunStatus::Blocked,
        RunStatus::Failed,
        RunStatus::Interrupted,
        RunStatus::Cancelled,
        RunStatus::Continued,
    ];
    for (status, saved) in statuses.iter().zip(SAVED) {
        assert_eq!(serde_json::to_string(status).unwrap(), format!("\"{saved}\""));
        assert_eq!(status.as_str(), saved);
        assert_eq!(status.to_string(), saved);
        assert_eq!(
            &serde_json::from_str::<RunStatus>(&format!("\"{saved}\"")).unwrap(),
            status
        );
    }
}

#[test]
fn saved_runs_load_and_save_back_byte_for_byte() {
    // The shape of scores/tests.rs's legacy record, saved in field order.
    let saved = |status: &str| {
        format!(
            r#"{{"spawns":[],"room_wake_used":false,"emitted":"{status}","persona":null,"delegation":null,"question":null,"continuation":null,"backend":"codex","model":null,"queue_order":0,"context":null,"lenses":null,"chat_id":"chat","attempts":[],"actor":{{"type":"unknown"}},"action":"task","id":"run","objective":"work","kind":"task","status":"{status}","source":"test","result":""}}"#
        )
    };
    // An unknown status (none is written today; none appears in fixtures)
    // survives a load and save unchanged and counts as finished.
    for status in SAVED.into_iter().chain(["paused-by-an-older-build", ""]) {
        let text = saved(status);
        let run: Record = serde_json::from_str(&text).unwrap();
        assert_eq!(serde_json::to_string(&run).unwrap(), text);
        assert_eq!(run.emitted.as_ref(), Some(&run.status));
    }
    let unknown: Record = serde_json::from_str(&saved("paused-by-an-older-build")).unwrap();
    assert_eq!(
        unknown.status,
        RunStatus::Unrecognised("paused-by-an-older-build".into())
    );
    assert!(unknown.status.is_finished() && !unknown.status.is_resumable());
    assert!(!unknown.status.is_stopped() && !unknown.status.is_unstopped());
}

#[test]
fn run_status_predicates_keep_their_sets() {
    let set = |test: fn(&RunStatus) -> bool| {
        SAVED
            .into_iter()
            .filter(|saved| test(&serde_json::from_str(&format!("\"{saved}\"")).unwrap()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        set(RunStatus::is_active),
        ["queued", "running", "cancelling", "waiting"]
    );
    assert_eq!(
        set(RunStatus::is_finished),
        [
            "completed",
            "blocked",
            "failed",
            "interrupted",
            "cancelled",
            "continued"
        ]
    );
    assert_eq!(
        set(RunStatus::is_resumable),
        ["blocked", "failed", "interrupted", "cancelled"]
    );
    assert_eq!(set(RunStatus::is_proceeding), ["queued", "running", "waiting"]);
    assert_eq!(set(RunStatus::occupies_slot), ["running", "cancelling"]);
    assert_eq!(
        set(RunStatus::is_stopped),
        ["cancelling", "blocked", "failed", "interrupted", "cancelled"]
    );
    assert_eq!(
        set(RunStatus::is_unstopped),
        ["queued", "running", "waiting", "completed", "continued"]
    );
}
