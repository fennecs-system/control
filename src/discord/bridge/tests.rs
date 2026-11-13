
use super::*;
#[test]
fn bridge_checkpoint_preserves_existing_progress_on_temporary_file_conflict() {
    let root = std::env::temp_dir().join(format!(
        "control-bridge-write-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let path = root.join("progress.json");
    checkpoint(&path, b"old").unwrap();
    let temporary = root.join(format!("progress.json.{}.tmp", std::process::id()));
    std::os::unix::fs::symlink(&path, &temporary).unwrap();
    assert!(checkpoint(&path, b"new").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"old");
    fs::remove_file(temporary).unwrap();
    checkpoint(&path, b"new").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"new");
    fs::remove_dir_all(root).unwrap();
}
fn id(s: &str) -> DiscordId {
    s.parse().unwrap()
}
#[test]
fn delivery_reserves_space_for_actual_changes_without_greening_or_losing_link() {
    let change = crate::outbox::Change {
        text: "1 file · +1 −1".into(),
        output: "@@ -1 +1 @@\n-old\n+new\n".into(),
    };
    let message = delivery_message(
        Some(&change),
        "completed",
        "http://localhost/chat/test",
        &"reply ".repeat(1000),
    );
    assert!(message.chars().count() <= 1800);
    assert!(message.contains("http://localhost/chat/test") && message.contains("```text\n"));
    assert!(message.contains("-old\n+new") && message.ends_with("```"));
    assert!(!delivery_message(Some(&change), "waiting", "link", "Question").contains("```"));
}
#[test]
fn design_reports_keep_answer_identity_and_all_options_within_dm_budget() {
    let report = design_report(&json!({"id":"q-test","input":{"question":"q".repeat(1000),"options":["a".repeat(200),"b".repeat(200),"c".repeat(200),"d".repeat(200)],"recommended":3}})).unwrap();
    assert!(report.starts_with("Reply /answer q-test"));
    assert!(report.contains("4. ") && report.contains("(recommended)"));
    assert!(report.chars().count() < 1500);
    assert!(design_report(&json!({"id":"bad\nidentity"})).is_none());
}
#[test]
fn dm_setup_checks_type_and_recipient() {
    assert_eq!(
        dm_channel(
            &json!({"id":"100","type":1,"recipients":[{"id":"2"}]}),
            id("2")
        ),
        Some(id("100"))
    );
    for row in [
        json!({"id":"100","type":3,"recipients":[{"id":"2"}]}),
        json!({"id":"100","type":1,"recipients":[{"id":"3"}]}),
    ] {
        assert!(dm_channel(&row, id("2")).is_none());
    }
}
#[test]
fn direct_messages_cannot_impersonate_another_recipient() {
    let route = Route {
        channel: id("100"),
        recipient: Some(id("2")),
    };
    let mut row = json!({"id":"10","channel_id":"100","author":{"id":"2"},"content":"hello"});
    assert!(incoming(&row, &route, id("99")).unwrap().direct);
    row["author"]["id"] = json!("1");
    assert!(incoming(&row, &route, id("99")).is_none());
    row["author"]["id"] = json!("2");
    row["author"]["bot"] = json!(true);
    assert!(incoming(&row, &route, id("99")).is_none());
}
#[test]
fn oversized_and_attachment_only_dms_get_a_reason() {
    let route = Route {
        channel: id("100"),
        recipient: Some(id("2")),
    };
    let mut row =
        json!({"id":"10","channel_id":"100","author":{"id":"2"},"content":"x".repeat(2001)});
    assert!(incoming(&row, &route, id("99")).is_none());
    assert!(rejected(&row, &route, id("99")).unwrap().contains("2000"));
    row["content"] = json!("");
    row["attachments"] = json!([{"id":"1"}]);
    assert!(
        rejected(&row, &route, id("99"))
            .unwrap()
            .contains("attachments")
    );
    row["author"]["id"] = json!("3");
    assert!(rejected(&row, &route, id("99")).is_none());
}
#[test]
fn state_cache_reparses_only_after_the_file_changes() {
    let path =
        std::env::temp_dir().join(format!("control-bridge-cache-{}.json", std::process::id()));
    fs::write(&path, br#"{"discord_seen":["1"]}"#).unwrap();
    let mut cache = StateCache::default();
    let first = cache.load(&path);
    assert!(first.discord_seen.contains("1"));
    assert!(std::rc::Rc::ptr_eq(&first, &cache.load(&path)));
    fs::write(&path, br#"{"discord_seen":["22"]}"#).unwrap();
    assert!(cache.load(&path).discord_seen.contains("22"));
    fs::remove_file(&path).unwrap();
    assert!(cache.load(&path).discord_seen.is_empty());
}
#[test]
fn state_cache_detects_atomic_replacement_and_refuses_symlinks() {
    let root = std::env::temp_dir().join(format!("control-bridge-replace-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("state.json");
    let replacement = root.join("replacement.json");
    fs::write(&path, br#"{"discord_seen":["1"]}"#).unwrap();
    let stamp = fs::metadata(&path).unwrap().modified().unwrap();
    let mut cache = StateCache::default();
    assert!(cache.load(&path).discord_seen.contains("1"));
    fs::write(&replacement, br#"{"discord_seen":["2"]}"#).unwrap();
    fs::File::options()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(stamp))
        .unwrap();
    fs::rename(&replacement, &path).unwrap();
    assert!(cache.load(&path).discord_seen.contains("2"));
    fs::rename(&path, &replacement).unwrap();
    std::os::unix::fs::symlink(&replacement, &path).unwrap();
    assert!(cache.load(&path).discord_seen.is_empty());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn server_messages_are_rejected_even_with_a_prefix() {
    let route = Route {
        channel: id("100"),
        recipient: None,
    };
    let mut row = json!({"id":"10","channel_id":"100","author":{"id":"2"},"content":"hello"});
    assert!(incoming(&row, &route, id("99")).is_none());
    row["content"] = json!("!control inspect this conversation");
    assert!(incoming(&row, &route, id("99")).is_none());
    assert!(route.actor(id("2")).is_none());
}
