
use super::*;

fn watch() -> Watch {
    Watch {
        id: "w-1".into(),
        name: "chat".into(),
        chat: "c".into(),
        service: "svc".into(),
        generation: 1,
        permanent: false,
        command: "tail -F log".into(),
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
fn notification_pages_do_not_spend_context_on_logs_or_hide_source_loss() {
    let mut frames = (1..=1000)
        .map(|seq| {
            (
                Frame {
                    seq,
                    event: Event::Line(b"diagnostic".to_vec()),
                },
                false,
            )
        })
        .collect::<Vec<_>>();
    frames.push((
        Frame {
            seq: 1001,
            event: Event::Notification(b"task-54 completed".to_vec()),
        },
        false,
    ));
    let page = select_page(frames.clone(), 0, 1001, false, 200);
    assert_eq!((page.events.len(), page.cursor, page.gap), (1, 1001, false));
    assert!(matches!(page.events[0].0.event, Event::Notification(_)));
    let logs = select_page(frames.clone(), 0, 1001, true, 200);
    assert_eq!(
        (logs.events.len(), logs.cursor, logs.gap),
        (200, 200, false)
    );
    frames.remove(400);
    assert!(
        select_page(frames, 0, 1001, false, 200).gap,
        "a missing frame could have been a notification"
    );
    let page = select_page(
        vec![(
            Frame {
                seq: 1,
                event: Event::Notification(vec![b'x'; 4096]),
            },
            true,
        )],
        0,
        1,
        false,
        200,
    );
    assert!(
        page.gap,
        "truncated notification evidence requires resynchronisation"
    );
}

#[test]
fn selection_survives_restart_without_moving_evidence_or_selected_cursor() {
    let mut watch = watch();
    watch.wake_policy = WakePolicy::Prefixes {
        prefixes: vec!["event:".into()],
    };
    for (seq, text) in [
        (1, "status: unchanged"),
        (2, "event: ready"),
        (3, "status: still unchanged"),
    ] {
        watch.observe(&Frame {
            seq,
            event: Event::Line(text.as_bytes().to_vec()),
        });
    }
    assert_eq!(
        (watch.seen, watch.line_seq, watch.wake_seq, watch.lines),
        (3, 3, Some(2), 1)
    );
    assert_eq!(watch.recent, ["event: ready"]);
    let mut restored: Watch = serde_json::from_slice(&serde_json::to_vec(&watch).unwrap()).unwrap();
    assert!(!restored.observe(&Frame {
        seq: 2,
        event: Event::Line(b"event: ready".to_vec())
    }));
    restored.observe(&Frame {
        seq: 4,
        event: Event::Line(b"status: unchanged".to_vec()),
    });
    restored.observe_for_test(5, Some(0));
    assert_eq!(
        (restored.lines, restored.wake_seq, restored.exits.len()),
        (1, Some(2), 1)
    );
    assert!(!restored.active());

    let mut legacy = serde_json::to_value(watch).unwrap();
    for key in ["wake_policy", "wake_seq", "gap", "created_by"] {
        legacy.as_object_mut().unwrap().remove(key);
    }
    let mut legacy: Watch = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.wake_policy, WakePolicy::AnyLine {});
    legacy.observe_for_test(4, None);
    assert_eq!(legacy.lines, 2, "old state retains any-line behavior");
}

#[test]
fn filtering_keeps_raw_spool_and_reports_missing_evidence() {
    let dir = std::env::temp_dir().join(format!(
        "control-watch-policy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("w.log");
    let mut watch = watch();
    watch.wake_policy = WakePolicy::ExitOnly {};
    let input = [(1, "status: unchanged"), (2, "event: inspectable")].map(|(seq, text)| Frame {
        seq,
        event: Event::Line(text.as_bytes().to_vec()),
    });
    fs::write(
        &path,
        input.iter().flat_map(Frame::encode).collect::<Vec<_>>(),
    )
    .unwrap();
    for frame in &input {
        watch.observe(frame);
    }
    assert_eq!(watch.lines, 0);
    let page = read_frames(&path, 0, 1);
    assert_eq!(page[0].0, input[0]);
    assert!(
        !source_gap(&page, 0, watch.seen, 1),
        "a full page is not a gap"
    );
    assert!(
        source_gap(&page, 0, watch.seen, 200),
        "a missing tail is a gap"
    );
    assert_eq!(read_frames(&path, 1, 200)[0].0, input[1]);
    assert_eq!(
        watch.lines, 0,
        "reading neither selects nor acknowledges events"
    );
    fs::remove_file(&path).unwrap();
    assert!(source_gap(&read_frames(&path, 0, 200), 0, watch.seen, 200));
    assert!(!source_gap(&[], watch.seen, watch.seen, 200));
    watch.observe(&Frame {
        seq: 5,
        event: Event::Line(b"status: silent gap".to_vec()),
    });
    assert_eq!(
        watch.gap,
        Some(Gap {
            after: 2,
            through: 4
        })
    );
    watch.observe_for_test(6, Some(1));
    assert_eq!(
        (watch.lines, watch.exits.len()),
        (0, 1),
        "gaps and exits bypass line selection"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn observed_frames_advance_once_and_a_transient_exit_ends_the_watch() {
    let mut watch = watch();
    assert!(watch.observe(&Frame {
        seq: 1,
        event: Event::Line(b"hello".to_vec())
    }));
    assert!(
        !watch.observe(&Frame {
            seq: 1,
            event: Event::Line(b"hello".to_vec())
        }),
        "replays are ignored"
    );
    assert!(watch.observe(&Frame {
        seq: 2,
        event: Event::Line(b"bye".to_vec())
    }));
    assert!(watch.active());
    assert!(watch.observe(&Frame {
        seq: 3,
        event: Event::Exit {
            code: Some(0),
            signal: None,
            restarts: 0,
            gave_up: false
        }
    }));
    assert_eq!(
        (watch.seen, watch.lines, watch.line_seq, watch.exits.len()),
        (3, 2, 2, 1)
    );
    assert!(!watch.active(), "a transient watch ends with its exit");
    let mut permanent = self::watch();
    permanent.permanent = true;
    permanent.observe(&Frame {
        seq: 1,
        event: Event::Exit {
            code: Some(1),
            signal: None,
            restarts: 0,
            gave_up: false,
        },
    });
    assert!(permanent.active(), "a permanent watch restarts");
    permanent.observe(&Frame {
        seq: 2,
        event: Event::Exit {
            code: Some(1),
            signal: None,
            restarts: 10,
            gave_up: true,
        },
    });
    assert!(!permanent.active(), "until it gives up");
}
