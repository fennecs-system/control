use control_agent::mailbox_wake::MailboxWake;
use std::time::Duration;

#[test]
#[ignore = "requires the pinned worker image and Docker; writes an isolated temporary mailbox, no credentials/models"]
fn container_mount_rename_reaches_host_watcher() {
    use std::os::unix::fs::MetadataExt;
    let image = crate::runtime::worker_image().expect("pinned worker image");
    let directory = std::env::temp_dir().join(format!("control-mount-wake-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let metadata = std::fs::metadata(&directory).unwrap();
    let wake = MailboxWake::new(&directory);
    assert!(wake.notifies());
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
    control_isolation::bounded_output(&mut command, Duration::from_secs(30), false).unwrap();
    let started = std::time::Instant::now();
    wake.wait(Duration::from_secs(3));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "Docker mount did not deliver a directory notification"
    );
    assert_eq!(std::fs::read(directory.join("request")).unwrap(), b"test");
    std::fs::remove_dir_all(directory).unwrap();
}
