
use super::*;
fn watched(id: &str, chat: &str) -> Watch {
    Watch {
        id: id.into(),
        name: "task".into(),
        chat: chat.into(),
        service: "svc-test".into(),
        generation: 1,
        permanent: false,
        command: "task".into(),
        stopped: false,
        wake_policy: Default::default(),
        notification_channel: false,
        notifications: None,
        created_by: None,
        seen: 0,
        lines: 0,
        line_seq: 0,
        wake_seq: None,
        gap: None,
        exits: Vec::new(),
        last_exit: None,
        recent: Vec::new(),
    }
}
#[test]
fn a_failed_arm_restores_unoffered_terminal_evidence() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Process each stage", "test").unwrap();
    let mut watch = watched("w-live", &chat);
    watch.permanent = true;
    watch.observe_for_test(1, None);
    watch.observe(&crate::watch_frame::Frame {
        seq: 2,
        event: Event::Notification(b"task complete".to_vec()),
    });
    watch.observe_for_test(4, Some(1));
    state.watches.push(watch);
    assert!(arm_saved(&mut state, &chat, |_| Err(io::Error::other("disk full"))).is_err());
    assert!(!crate::bots::defined(&state, &chat));
    assert_eq!(state.watches[0].lines, 1);
    assert_eq!(state.watches[0].exits.len(), 1);
    assert_eq!(state.watches[0].pending_notifications(), 1);
    assert_eq!(
        state.watches[0].gap,
        Some(super::super::Gap {
            after: 2,
            through: 3
        })
    );
    assert!(arm_saved(&mut state, &chat, |_| Ok(())).unwrap());
}
#[test]
fn completed_evidence_is_retained_until_old_and_unreferenced() {
    let mut watches = (0..18)
        .map(|i| {
            let mut w = watched(&format!("w-{i}"), "c-owner");
            w.observe_for_test(1, Some(0));
            w.exits.clear();
            w
        })
        .collect::<Vec<_>>();
    watches[0].lines = 1;
    assert!(
        retire(&mut watches, "c-owner", |id| id == "w-1").is_empty(),
        "unoffered and held evidence survives beside the last sixteen records"
    );
    let retired = retire(&mut watches, "c-owner", |_| false);
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].id, "w-1");
    assert!(watches.iter().any(|w| w.id == "w-0"));
    assert!(
        retire(&mut watches, "c-other", |_| false).is_empty(),
        "another chat's history is unaffected"
    );
}
