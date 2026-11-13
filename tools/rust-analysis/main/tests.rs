
use super::*;
#[test]
fn exports_sources_without_credentials_or_symlinks() {
    let source = Snapshot::new().unwrap();
    let target = Snapshot::new().unwrap();
    fs::write(source.0.join("Cargo.toml"), "[package]").unwrap();
    fs::write(source.0.join("main.rs"), "fn main() {}").unwrap();
    fs::write(source.0.join(".env"), "fake-secret").unwrap();
    fs::create_dir(source.0.join(".codex")).unwrap();
    fs::write(source.0.join(".codex/auth.json"), "fake-secret").unwrap();
    std::os::unix::fs::symlink(source.0.join(".env"), source.0.join("leak.rs")).unwrap();
    let mut totals = (0, 0);
    export(&source.0, &target.0, &mut totals).unwrap();
    assert_eq!(totals.0, 2);
    assert!(!target.0.join(".env").exists());
    assert!(!target.0.join(".codex").exists());
    assert!(!target.0.join("leak.rs").exists());
}
#[test]
fn rejects_mutable_image_tags() {
    assert!(!valid_image("control-analysis:nix"));
    assert!(!valid_image("sha256:abc"));
    assert!(valid_image(&format!("sha256:{}", "a".repeat(64))));
}
