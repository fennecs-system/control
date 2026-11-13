
use super::*;
#[test]
fn invalid_authority_and_viewports_fail_before_docker() {
    assert!(
        capture(
            "control-browser:nix",
            Path::new("missing"),
            1280,
            800,
            Path::new("unused")
        )
        .is_err()
    );
    assert!(
        capture(
            &format!("sha256:{}", "a".repeat(64)),
            Path::new("missing"),
            1,
            800,
            Path::new("unused")
        )
        .is_err()
    );
    assert!(valid_image(&format!("sha256:{}", "a".repeat(64))));
    assert!(!valid_image("sha256:xyz"));
}
#[cfg(unix)]
#[test]
fn input_reader_rejects_symlinks_and_nonregular_files() {
    use std::os::unix::fs::symlink;
    let dir = std::env::temp_dir().join(format!("control-browser-input-{}", std::process::id()));
    private_dir(&dir).unwrap();
    private_file(&dir.join("page.html"), b"<p>Preview</p>").unwrap();
    symlink(dir.join("page.html"), dir.join("link.html")).unwrap();
    assert!(read_input(&dir.join("link.html")).is_err());
    assert!(read_input(&dir).is_err());
    assert_eq!(
        read_input(&dir.join("page.html")).unwrap(),
        b"<p>Preview</p>"
    );
    let _ = fs::remove_dir_all(dir);
}
