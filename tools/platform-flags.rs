// These std-only container entrypoints are also compiled directly by rustc.
// Keep supported targets explicit: Linux open(2) constants differ by CPU ABI.
#[cfg(target_os = "macos")]
pub const SAFE_READ: i32 = 0x100 | 0x4;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const SAFE_READ: i32 = 0x8000 | 0x800;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const SAFE_READ: i32 = 0x20000 | 0x800;
#[cfg(not(any(
    target_os = "macos",
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
compile_error!("Safe file reads require explicit flags for this operating system and CPU ABI");

#[cfg(test)]
mod tests {
    #[test]
    fn native_flags_reject_a_symlink_and_accept_its_regular_target() {
        use std::{fs, os::unix::fs::{symlink, OpenOptionsExt}, time::{SystemTime, UNIX_EPOCH}};
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("control-platform-flags-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let file = root.join("regular");
        fs::write(&file, b"safe").unwrap();
        let link = root.join("link");
        symlink(&file, &link).unwrap();
        assert!(fs::OpenOptions::new().read(true).custom_flags(super::SAFE_READ).open(&link).is_err());
        assert!(fs::OpenOptions::new().read(true).custom_flags(super::SAFE_READ).open(&file).unwrap().metadata().unwrap().is_file());
        fs::remove_dir_all(root).unwrap();
    }
}
