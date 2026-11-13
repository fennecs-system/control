
use super::*;

#[test]
fn frames_round_trip_and_processes_cannot_forge_frames() {
    let hostile = b"chat\tX\t0 - 0 1\nnext\\n\r".to_vec();
    let line = Frame {
        seq: 7,
        event: Event::Line(hostile.clone()),
    }
    .encode();
    assert_eq!(
        line.iter().filter(|&&b| b == b'\n').count(),
        1,
        "one frame per line"
    );
    assert_eq!(
        Frame::decode(&line[..line.len() - 1]),
        Some((
            Frame {
                seq: 7,
                event: Event::Line(hostile)
            },
            false
        ))
    );
    let exit = Frame {
        seq: 8,
        event: Event::Exit {
            code: None,
            signal: Some(9),
            restarts: 3,
            gave_up: true,
        },
    };
    let encoded = exit.encode();
    assert_eq!(
        Frame::decode(&encoded[..encoded.len() - 1]),
        Some((exit, false))
    );
    let notice = Frame {
        seq: 9,
        event: Event::Notification(b"task-2\tX\t0 - 0 1\ncomplete\\n".to_vec()),
    };
    let encoded = notice.encode();
    assert_eq!(encoded.iter().filter(|&&b| b == b'\n').count(), 1);
    assert_eq!(
        Frame::decode(&encoded[..encoded.len() - 1]),
        Some((notice, false))
    );
    let long = Frame {
        seq: 10,
        event: Event::Notification(vec![b'n'; MAX_LINE + 1]),
    }
    .encode();
    assert_eq!(
        Frame::decode(&long[..long.len() - 1]),
        Some((
            Frame {
                seq: 10,
                event: Event::Notification(vec![b'n'; MAX_LINE])
            },
            true
        ))
    );
    assert!(
        Frame {
            seq: u64::MAX,
            event: Event::Line(vec![b'\\'; MAX_LINE + 1])
        }
        .encode()
        .len()
            <= MAX_FRAME
    );
    let long = Frame {
        seq: 9,
        event: Event::Line(vec![b'a'; MAX_LINE + 10]),
    }
    .encode();
    assert_eq!(
        Frame::decode(&long[..long.len() - 1]),
        Some((
            Frame {
                seq: 9,
                event: Event::Line(vec![b'a'; MAX_LINE])
            },
            true
        ))
    );
    for bad in [
        &b"x\tL\ta"[..],
        b"1\tQ\ta",
        b"1\tL\ta\\q",
        b"1\tX\t0 - 0",
        b"1\tX\t0 - 0 2",
        b"1\tL\ta\\.b",
    ] {
        assert_eq!(Frame::decode(bad), None, "{}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn specs_ids_and_backoff_are_bounded() {
    let spec = Spec {
        restart: Restart::Permanent,
        notifications: false,
        command: "tail -F log\n| grep x".into(),
    };
    assert_eq!(Spec::decode(&spec.encode()), Some(spec));
    let explicit = Spec {
        restart: Restart::Transient,
        notifications: true,
        command: "task".into(),
    };
    assert_eq!(Spec::decode(&explicit.encode()), Some(explicit));
    assert!(!Spec::decode("transient\ntask").unwrap().notifications);
    assert!(Spec::decode("transient notify-v2\ntask").is_none());
    assert_eq!(Spec::decode("sometimes\nls"), None);
    assert_eq!(Spec::decode("transient\n  "), None);
    assert!(
        valid_id("alice-chat") && !valid_id("../x") && !valid_id("") && !valid_id(&"a".repeat(33))
    );
    assert_eq!(
        (0..9).map(backoff).collect::<Vec<_>>(),
        [1, 2, 4, 8, 16, 32, 60, 60, 60]
    );
}
