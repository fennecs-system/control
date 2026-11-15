use super::*;

#[test]
fn borrowed_admission_requires_authority_generation_and_actionable_peer_evidence() {
    let mut origin = super::super::runs::Submission {
        objective: "Work".into(),
        kind: RunKind::Chat,
        source: "test".into(),
        actor: Actor::LocalOperator,
        action: Action::Task,
    }
    .into_run("origin".into(), 1, Default::default(), None);
    origin.chat_id = Some("chat".into());
    origin.status = "completed".into();
    origin.attempts.push(crate::usage::Attempt {
        workspace_id: Some("attempt".into()),
        ..Default::default()
    });
    let runs = [origin];
    let chats = [serde_json::from_value::<Session>(serde_json::json!({
        "id":"chat", "actor":Actor::LocalOperator, "title":"Work",
        "messages":[], "room":"research"
    }))
    .unwrap()];
    let query = |allowed, generation, posts: &[Post]| {
        plan(
            View {
                runs: &runs,
                chats: &chats,
                posts,
            },
            std::iter::once((
                7,
                WakeRef {
                    chat: "chat",
                    actor: &Actor::LocalOperator,
                    room: "research",
                    cursor: 0,
                    origin: "origin",
                    generation,
                    scanned: 0,
                },
            )),
            |_, _| allowed,
        )
    };
    let mut posts = vec![
        Post {
            room: "research".into(),
            text: "Own output".into(),
            chat: Some("chat".into()),
            run: None,
            key: String::new(),
            delivery: crate::rooms::Delivery::Notify,
        },
        Post {
            room: "research".into(),
            text: "Recorded observation".into(),
            chat: Some("peer".into()),
            run: None,
            key: String::new(),
            delivery: crate::rooms::Delivery::Record,
        },
    ];
    assert_eq!(
        query(true, "attempt", &posts),
        vec![(7, Decision::Advance(2))]
    );
    posts[1].delivery = crate::rooms::Delivery::Notify;
    assert_eq!(
        query(true, "attempt", &posts),
        vec![(7, Decision::Queue(0))]
    );
    assert_eq!(query(false, "attempt", &posts), vec![(7, Decision::Disarm)]);
    assert_eq!(query(true, "stale", &posts), vec![(7, Decision::Disarm)]);
}
