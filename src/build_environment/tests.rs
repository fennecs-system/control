use super::*;

fn locks() -> BTreeMap<String, String> {
    // SHA256 of the empty file, representing a deliberately empty fixture.
    parse(r#"{"Cargo.lock":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","web/Cargo.lock":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}"#).unwrap()
}

#[test]
fn matching_locks_pass_missing_optional_workspaces_do_not_block_and_stale_locks_explain_recovery() {
    matches(&locks(), |_| Ok(Some(Vec::new()))).unwrap();
    matches(&locks(), |path| Ok((path == "Cargo.lock").then(Vec::new))).unwrap();
    let error = matches(&locks(), |path| {
        Ok(Some(if path == "web/Cargo.lock" {
            b"changed".to_vec()
        } else {
            Vec::new()
        }))
    })
    .unwrap_err()
    .to_string();
    assert!(error.contains("stale for web/Cargo.lock") && error.contains("rebuild"));
    let error = matches(&locks(), |_| Err(io::Error::other("no-follow read failed"))).unwrap_err();
    assert!(error.to_string().contains("no-follow read failed"));
}

#[test]
fn absent_or_untrusted_image_metadata_cannot_select_host_paths() {
    for text in ["", "<no value>", "{}", "[]"] {
        assert!(parse(text).is_err());
    }
    for path in [
        "../Cargo.lock",
        "/Cargo.lock",
        "web/../Cargo.lock",
        "auth.json",
        "",
    ] {
        let record = BTreeMap::from([(path, locks()["Cargo.lock"].clone())]);
        assert!(
            parse(&serde_json::to_string(&record).unwrap()).is_err(),
            "{path}"
        );
    }
    for hash in ["bad".to_owned(), "A".repeat(64)] {
        assert!(parse(&serde_json::json!({"Cargo.lock":hash}).to_string()).is_err());
    }
}

#[test]
#[ignore = "requires CONTROL_QA_BUILD_IMAGE, a locally installed pinned build image; no models or credentials"]
fn live_image_metadata_matches_source_and_rejects_changed_locks_and_symlinks() {
    let image = std::env::var("CONTROL_QA_BUILD_IMAGE").unwrap();
    let root = std::env::temp_dir().join(format!("control-build-readiness-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _fixture = Fixture(root.clone());
    std::fs::write(root.join("Cargo.lock"), include_bytes!("../../Cargo.lock")).unwrap();
    check(&image, &root).unwrap();
    std::fs::write(root.join("Cargo.lock"), "changed lock").unwrap();
    assert!(
        check(&image, &root)
            .unwrap_err()
            .to_string()
            .contains("stale for Cargo.lock")
    );
    std::fs::remove_file(root.join("Cargo.lock")).unwrap();
    std::os::unix::fs::symlink(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock"),
        root.join("Cargo.lock"),
    )
    .unwrap();
    assert!(check(&image, &root).is_err());
}
