use super::*;

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
            crate::backend::Backend::Claude,
            model.clone(),
        );
        origin.status = "completed".into();
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
        let run = request("continue").into_continuation(
            &origin,
            "chat",
            "next".into(),
            9,
            vec![current.clone()],
        );
        assert_eq!(run.model, model);
        assert_eq!(run.backend, origin.backend);
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
        assert_eq!(run.status, "queued");
        assert!(run.attempts.is_empty() && run.result.is_empty() && run.spawns.is_empty());
        assert!(run.question.is_none() && run.continuation.is_none() && run.delegation.is_none());
        assert!(run.bot_turn.is_none() && run.emitted.is_none() && !run.room_wake_used);
    }
}

#[test]
fn continuations_establish_lineage_when_the_origin_has_no_persona() {
    let origin = request("original").into_run("origin".into(), 1, Default::default(), None);
    let run = request("continue").into_continuation(&origin, "chat", "next".into(), 2, Vec::new());
    let persona = run.persona.unwrap();
    assert_eq!(persona.root, "origin");
    assert_eq!(persona.depth, 0);
    assert_eq!(persona.parent, None);
    assert_eq!(persona.role, crate::delegation::Role::Worker);
}
