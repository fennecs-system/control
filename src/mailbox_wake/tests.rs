use super::*;
#[test]
fn atomic_mailbox_rename_wakes_without_waiting_for_lifecycle_poll() {
    let directory =
        std::env::temp_dir().join(format!("control-mailbox-wake-{}", std::process::id()));
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

#[test]
#[ignore = "requires the pinned worker image and Docker; writes an isolated temporary mailbox, no credentials/models"]
fn container_mount_rename_reaches_host_watcher() {
    use std::os::unix::fs::MetadataExt;
    let image = crate::runtime::worker_image().expect("pinned worker image");
    let directory = std::env::temp_dir().join(format!("control-mount-wake-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let metadata = std::fs::metadata(&directory).unwrap();
    let wake = MailboxWake::new(&directory);
    assert!(wake.descriptor.is_some());
    let mut command = crate::engine::scrubbed();
    command.args([
        "run",
        "--rm",
        "--pull=never",
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--user",
        &format!("{}:{}", metadata.uid(), metadata.gid()),
        "--mount",
        &format!("type=bind,src={},dst=/mailbox", directory.display()),
        "--entrypoint",
        "/bin/sh",
        &image,
        "-c",
        "printf test > /mailbox/request.tmp && mv /mailbox/request.tmp /mailbox/request",
    ]);
    crate::runtime::engine_output(&mut command, Duration::from_secs(30)).unwrap();
    let started = std::time::Instant::now();
    wake.wait(Duration::from_secs(3));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "Docker mount did not deliver a directory notification"
    );
    assert_eq!(std::fs::read(directory.join("request")).unwrap(), b"test");
    std::fs::remove_dir_all(directory).unwrap();
}
