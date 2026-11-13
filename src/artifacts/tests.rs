
use super::*;
#[test]
fn live_diff_reads_stay_inside_pinned_directories_and_skip_private_outputs() {
    let root = std::env::temp_dir().join(format!(
        "control-diff-safe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let workspace = root.join("workspace");
    let outside = root.join("outside");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(workspace.join("src/main.rs"), "inside\n").unwrap();
    fs::write(outside.join("main.rs"), "outside-fixture\n").unwrap();
    fs::write(workspace.join(".env"), "private-fixture").unwrap();
    fs::write(workspace.join("private.key"), "private-key-fixture").unwrap();
    fs::write(workspace.join("binary.dat"), [0, 1, 2]).unwrap();
    fs::write(workspace.join("large.txt"), vec![b'x'; 128 * 1024 + 1]).unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join("linked")).unwrap();
    let snapshot = diff_snapshot(&workspace).unwrap();
    assert_eq!(snapshot.files.len(), 1);
    assert_eq!(snapshot.files["src/main.rs"], "inside\n");
    assert!(
        snapshot.skipped.contains("linked")
            && snapshot.skipped.contains("binary.dat")
            && snapshot.skipped.contains("large.txt")
    );
    let pinned = fs::File::open(workspace.join("src")).unwrap();
    fs::rename(workspace.join("src"), workspace.join("original-src")).unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join("src")).unwrap();
    let mut text = String::new();
    diff_open_child(&pinned, std::ffi::OsStr::new("main.rs"))
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    assert_eq!(text, "inside\n");
    let snapshot = diff_snapshot(&workspace).unwrap();
    assert!(
        snapshot
            .files
            .values()
            .all(|text| !text.contains("outside-fixture"))
    );
    assert!(diff_snapshot(&workspace.join("src")).is_err());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn output_browser_omits_empty_checkpoints_and_duplicate_versions() {
    let outputs = visible_outputs(["empty", "file", "empty", "file"], |id| {
        Ok(Manifest {
            id: id.into(),
            files: if id == "file" {
                vec![Entry {
                    path: "empty-but-real.txt".into(),
                    bytes: 0,
                    sha256: "0".repeat(64),
                    media_type: "text/plain".into(),
                }]
            } else {
                vec![]
            },
            omitted: vec![],
        })
    })
    .unwrap();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].files[0].bytes, 0);
    assert!(visible_outputs(["broken"], |_| Err(fail("bad manifest"))).is_err());
}
#[test]
fn artifact_tools_keep_chat_authority_and_reject_actor_overrides() {
    let mut state = crate::State::default();
    let first = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &first, "create an output", "test").unwrap();
    state.runtime.runs[0].attempts.push(crate::usage::Attempt {
        artifact: Some("output-first-chat".into()),
        ..Default::default()
    });
    let second = crate::chat::create(&mut state);
    let error = tool(
        &state,
        TOOLS[1],
        &serde_json::json!({"id":second,"artifact":"output-first-chat","path":"file.txt"}),
    )
    .unwrap_err();
    assert_eq!(error, "Artifact does not belong to this chat");
    state.chats[1].actor = crate::identity::Actor::Unknown;
    assert!(tool(&state, TOOLS[0], &serde_json::json!({"id":second})).is_err());
    assert!(
        tool(
            &state,
            TOOLS[0],
            &serde_json::json!({"id":second,"actor":{"type":"local_operator"}})
        )
        .is_err()
    );
    for definition in definitions() {
        assert_eq!(definition["annotations"]["readOnlyHint"], true);
        assert_eq!(definition["annotations"]["openWorldHint"], false);
    }
}
#[test]
fn followup_restores_published_files_and_detects_corruption() {
    let directory =
        std::env::temp_dir().join(format!("control-artifact-lineage-{}", std::process::id()));
    fs::create_dir_all(directory.join("first/src")).unwrap();
    fs::write(directory.join("first/src/main.rs"), "first turn").unwrap();
    let outputs = directory.join("outputs");
    publish_in(&outputs, &directory.join("first"), "attempt-1").unwrap();
    restore_in(&outputs, "attempt-1", &directory.join("second")).unwrap();
    assert_eq!(
        fs::read_to_string(directory.join("second/src/main.rs")).unwrap(),
        "first turn"
    );
    fs::write(directory.join("second/src/main.rs"), "second turn").unwrap();
    publish_in(&outputs, &directory.join("second"), "attempt-2").unwrap();
    restore_in(&outputs, "attempt-2", &directory.join("after-restart")).unwrap();
    assert_eq!(
        fs::read_to_string(directory.join("after-restart/src/main.rs")).unwrap(),
        "second turn"
    );
    let first = manifest_in(&outputs, "attempt-1").unwrap();
    assert_eq!(
        read_in(&outputs, &first, "src/main.rs").unwrap(),
        b"first turn"
    );
    fs::write(outputs.join("attempt-1/files/src/main.rs"), "changed").unwrap();
    assert!(read_in(&outputs, &first, "src/main.rs").is_err());
    fs::remove_dir_all(directory).unwrap();
}
#[test]
fn publication_is_bounded_sorted_and_never_promotes_symlinks_or_private_files() {
    let root = std::env::temp_dir().join(format!("control-artifacts-{}", std::process::id()));
    fs::create_dir_all(root.join("source/src")).unwrap();
    fs::write(root.join("source/src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(root.join("source/.env"), "not-a-real-secret").unwrap();
    fs::write(root.join("source/.control-result"), "harness-only").unwrap();
    let manifest = publish_in(&root.join("output"), &root.join("source"), "test-1").unwrap();
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(manifest.files[0].path, "src/main.rs");
    assert_eq!(manifest.files[0].sha256, hash(b"fn main() {}\n"));
    assert!(!root.join("output/test-1/files/.env").exists());
    assert!(publish_in(&root.join("output"), &root.join("source"), "test-1").is_err());
    std::os::unix::fs::symlink("/etc/passwd", root.join("source/link")).unwrap();
    // Generated output is excluded by policy; unsafe project entries reject publication.
    fs::create_dir_all(root.join("source/dist")).unwrap();
    fs::write(root.join("source/dist/bundle.js"), "built").unwrap();
    let error = publish_in(&root.join("output"), &root.join("source"), "test-2")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("link: Symlink or special file"));
    assert!(!root.join("output/test-2").exists());
    assert!(safe_read(&root.join("source"), "../source/src/main.rs", FILE_BYTES).is_err());
    assert!(safe_read(&root.join("source"), "link", FILE_BYTES).is_err());
    fs::remove_file(root.join("source/link")).unwrap();
    fs::File::create(root.join("source/large"))
        .unwrap()
        .set_len(FILE_BYTES + 1)
        .unwrap();
    let error = publish_in(&root.join("output"), &root.join("source"), "test-3")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("large: Larger than the 2 MiB file limit"));
    assert!(!root.join("output/test-3").exists());
    fs::remove_file(root.join("source/large")).unwrap();
    for n in 0..9 {
        fs::File::create(root.join(format!("source/big-{n}")))
            .unwrap()
            .set_len(FILE_BYTES)
            .unwrap();
    }
    let error = publish_in(&root.join("output"), &root.join("source"), "test-4")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("16 MiB total limit"));
    assert!(!root.join("output/test-4").exists());
    restore_in(&root.join("output"), "test-1", &root.join("restored")).unwrap();
    assert_eq!(
        fs::read(root.join("restored/src/main.rs")).unwrap(),
        b"fn main() {}\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn incomplete_turn_never_replaces_the_restorable_project_checkpoint() {
    let root =
        std::env::temp_dir().join(format!("control-checkpoint-limits-{}", std::process::id()));
    let source = root.join("source");
    let outputs = root.join("outputs");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("project.txt"), "original project data").unwrap();
    publish_in(&outputs, &source, "before").unwrap();
    let workspace = root.join("turn-two");
    restore_in(&outputs, "before", &workspace).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(workspace.join("project.txt"))
        .unwrap()
        .set_len(FILE_BYTES + 1)
        .unwrap();
    let error = publish_in(&outputs, &workspace, "oversized")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("project.txt") && error.contains("incomplete workspace checkpoint"));
    assert!(!outputs.join("oversized").exists());
    assert_eq!(
        fs::metadata(workspace.join("project.txt")).unwrap().len(),
        FILE_BYTES + 1
    );
    fs::write(workspace.join("project.txt"), "updated project data").unwrap();
    for n in 0..FILE_COUNT {
        fs::write(workspace.join(format!("file-{n:04}")), "x").unwrap();
    }
    let error = publish_in(&outputs, &workspace, "too-many")
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("512-file limit"));
    assert!(!outputs.join("too-many").exists());
    let next = root.join("next-turn");
    restore_in(&outputs, "before", &next).unwrap();
    assert_eq!(
        fs::read(next.join("project.txt")).unwrap(),
        b"original project data"
    );
    fs::remove_dir_all(root).unwrap();
}
