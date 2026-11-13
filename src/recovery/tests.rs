
use super::*;
#[test]
#[ignore = "requires pinned worker image, Docker and isolated CONTROL_STATE_DIR; no model calls"]
fn live_stopped_writer_leaves_recovery_bytes_without_a_model_handoff() {
    let state_root = std::env::var("CONTROL_STATE_DIR").expect("isolated state directory");
    assert!(
        state_root.starts_with("/private/tmp/control-r1-")
            || state_root.starts_with("/tmp/control-r1-")
    );
    let image = crate::runtime::worker_image().expect("pinned worker image");
    let generation = format!("r1-fixture-{}", std::process::id());
    let workspace = crate::runtime::workspace(&generation).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    let source = workspace.canonicalize().unwrap();
    let name = format!("control-{generation}");
    let user = format!("{}:{}", unsafe { libc::geteuid() }, unsafe {
        libc::getegid()
    });
    let id = crate::engine::query(&[
        "run",
        "--detach",
        "--rm",
        "--pull=never",
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--name",
        &name,
        "--user",
        &user,
        "--label",
        "dev.control.node=r1-fixture",
        "--label",
        &format!("dev.control.attempt={name}"),
        "--label",
        crate::lifecycle::CHECK_LABEL,
        "--mount",
        &format!("type=bind,src={},dst=/workspace", source.display()),
        &image,
        "bash",
        "-c",
        "printf 'retained useful work' > /workspace/notes.txt; sleep 120",
    ])
    .unwrap();
    for _ in 0..50 {
        if source.join("notes.txt").exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let mut attempt = Attempt {
        workspace_id: Some(generation.clone()),
        node_id: Some("r1-fixture".into()),
        image: Some(image),
        termination: Some(Termination::TimedOut),
        ..Default::default()
    };
    capture(None, &mut attempt);
    let running =
        crate::engine::query(&["ps", "--quiet", "--filter", &format!("id={}", id.trim())]).unwrap();
    // Cleanup even when the assertion below reports an ownership/copy failure.
    if !running.trim().is_empty() {
        let _ = crate::engine::query(&["rm", "-f", id.trim()]);
    }
    let candidate = attempt.recovery.unwrap();
    assert!(running.trim().is_empty(), "writer must stop before capture");
    assert!(candidate.captured, "{:?}", candidate.error);
    assert!(candidate.draft.is_none());
    assert!(attempt.artifact.is_none());
    let retained = directory(&root().unwrap(), &generation).unwrap();
    assert_eq!(
        fs::read_to_string(retained.join("workspace/notes.txt")).unwrap(),
        "retained useful work"
    );
    fs::write(source.join("notes.txt"), "later unrelated changes").unwrap();
    assert_eq!(
        fs::read_to_string(retained.join("workspace/notes.txt")).unwrap(),
        "retained useful work"
    );
}
#[test]
fn missing_source_never_creates_a_completed_candidate() {
    let root =
        std::env::temp_dir().join(format!("control-recovery-missing-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let candidate = Candidate {
        generation: "attempt-1".into(),
        base: None,
        project: None,
        project_base: None,
        cause: Termination::Restarted,
        captured: false,
        error: None,
        draft: None,
    };
    assert!(snapshot(&root, &root.join("missing"), &candidate).is_err());
    assert!(!root.join("attempt-1").exists());
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn recovery_tool_never_crosses_chat_scope_or_restores_a_changed_base() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Recover my work", "test").unwrap();
    crate::worker::checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let id = state.runtime.runs[0].id.clone();
    let generation = state.runtime.runs[0].attempts[0]
        .workspace_id
        .clone()
        .unwrap();
    state.runtime.runs[0].attempts[0].recovery = Some(Candidate {
        generation: "old".into(),
        base: Some("different-base".into()),
        project: None,
        project_base: None,
        cause: Termination::TimedOut,
        captured: true,
        error: None,
        draft: None,
    });
    let foreign = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &foreign, "Other work", "test").unwrap();
    state.runtime.runs[1].attempts.push(Attempt {
        recovery: Some(Candidate {
            generation: "foreign".into(),
            base: None,
            project: None,
            project_base: None,
            cause: Termination::Cancelled,
            captured: false,
            error: None,
            draft: None,
        }),
        ..Default::default()
    });
    let shared = Arc::new(Mutex::new(state));
    let host = crate::tool_registry::LocalHost(&shared);
    let cx = AttemptCall {
        host: &host,
        attempt: crate::tool_registry::AttemptRef {
            run: &id,
            generation: &generation,
            nonce: "read",
        },
    };
    assert!(
        Tools
            .call(&cx, "control_recovery", &json!({"generation":"foreign"}))
            .is_err()
    );
    assert!(
        Tools
            .call(
                &cx,
                "control_recovery",
                &json!({"generation":"old","restore":true})
            )
            .unwrap_err()
            .to_string()
            .contains("base")
    );
    let page = Tools.call(&cx, "control_recovery", &json!({})).unwrap();
    assert_eq!(page["total"], 1);
    assert_eq!(page["candidates"][0]["generation"], "old");
}
#[test]
fn candidate_retains_bytes_independently_without_publishing_or_following_links() {
    let root = std::env::temp_dir().join(format!("control-recovery-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("live")).unwrap();
    fs::write(root.join("live/notes.txt"), "useful work").unwrap();
    fs::write(root.join("live/large.bin"), vec![3u8; 3 * 1024 * 1024]).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", root.join("live/link")).unwrap();
    let candidate = Candidate {
        generation: "attempt-1".into(),
        base: Some("old".into()),
        project: None,
        project_base: None,
        cause: Termination::TimedOut,
        captured: false,
        error: None,
        draft: None,
    };
    snapshot(&root.join("retained"), &root.join("live"), &candidate).unwrap();
    fs::write(root.join("live/notes.txt"), "later edit").unwrap();
    let saved = root.join("retained/attempt-1/workspace");
    assert_eq!(
        fs::read_to_string(saved.join("notes.txt")).unwrap(),
        "useful work"
    );
    assert_eq!(
        fs::metadata(saved.join("large.bin")).unwrap().len(),
        3 * 1024 * 1024
    );
    assert!(!saved.join("link").exists());
    snapshot(&root.join("retained"), &root.join("live"), &candidate).unwrap();
    assert_eq!(
        fs::read_to_string(saved.join("notes.txt")).unwrap(),
        "useful work"
    );
    let mut moved = candidate;
    moved.base = Some("new".into());
    assert!(snapshot(&root.join("retained"), &root.join("live"), &moved).is_err());
    fs::remove_dir_all(root).unwrap();
}
