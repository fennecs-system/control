
use super::*;
use std::io::Write;

#[test]
fn followers_read_whole_events_from_their_cursor_and_notice_rewrites() {
    let path = std::env::temp_dir().join(format!("control-outbox-{}.jsonl", std::process::id()));
    let event = |run: &str, status: &str| {
        serde_json::to_string(&Delivery {
            run: run.into(),
            source: "discord:dm".into(),
            chat: None,
            actor: Actor::LocalOperator,
            action: Action::Task,
            status: status.into(),
            report: "done".into(),
            question: Value::Null,
            change: None,
        })
        .unwrap()
    };
    let runs = |events: &[(Delivery, Cursor)]| {
        events
            .iter()
            .map(|(e, _)| e.run.clone())
            .collect::<Vec<_>>()
    };
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n{{\"run\":\"tor",
            event("a", "completed"),
            event("b", "waiting")
        ),
    )
    .unwrap();
    let events = read(&path, Cursor::default()).unwrap();
    assert_eq!(runs(&events), ["a", "b"], "a torn final line waits");
    let after_b = events[1].1;
    // The writer rolls back b and appends a longer, different event in its place.
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(events[0].1.offset).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    writeln!(file, "{}", event("c", "completed-after-a-long-wait")).unwrap();
    assert_eq!(
        runs(&read(&path, after_b).unwrap()),
        ["a", "c"],
        "a rewritten tail restarts reading; receipts skip a"
    );
    let all = read(&path, Cursor::default()).unwrap();
    assert!(read(&path, all.last().unwrap().1).unwrap().is_empty());
    let _ = std::fs::remove_file(&path);
}
