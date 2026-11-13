
#[test]
fn native_flags_reject_a_symlink_and_accept_its_regular_target() {
    use std::{
        fs,
        os::unix::fs::{OpenOptionsExt, symlink},
        time::{SystemTime, UNIX_EPOCH},
    };
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "control-platform-flags-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&root).unwrap();
    let file = root.join("regular");
    fs::write(&file, b"safe").unwrap();
    let link = root.join("link");
    symlink(&file, &link).unwrap();
    assert!(
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(super::SAFE_READ)
            .open(&link)
            .is_err()
    );
    assert!(
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(super::SAFE_READ)
            .open(&file)
            .unwrap()
            .metadata()
            .unwrap()
            .is_file()
    );
    fs::remove_dir_all(root).unwrap();
}
