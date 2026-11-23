use super::*;
#[test]
fn atomic_mailbox_rename_wakes_without_waiting_for_lifecycle_poll() {
    let directory = std::env::temp_dir().join(format!("control-mailbox-wake-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let wake = MailboxWake::new(&directory);
    assert!(wake.descriptor.is_some());
    std::fs::write(directory.join("request.tmp"), b"test").unwrap();
    std::fs::rename(directory.join("request.tmp"), directory.join("request")).unwrap();
    let started = std::time::Instant::now();
    wake.wait(Duration::from_secs(3));
    assert!(started.elapsed() < Duration::from_secs(1));
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
fn unavailable_watch_falls_back_to_bounded_lifecycle_poll() {
    let wake = MailboxWake::new(Path::new("/control-missing-mailbox-directory"));
    assert!(wake.descriptor.is_none());
    let started = std::time::Instant::now();
    wake.wait(Duration::from_millis(10));
    assert!(started.elapsed() >= Duration::from_millis(10));
}
