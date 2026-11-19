
use super::*;
use std::os::unix::fs::PermissionsExt;
struct Fixture(PathBuf);

#[test]
fn pending_project_files_survive_uncertain_unwind_but_not_ordinary_rejection() {
    let fixture = Fixture::new();
    let path = fixture.0.join("pending-grant");
    fs::create_dir(&path).unwrap();
    fs::write(path.join("checkpoint.txt"), "candidate bytes").unwrap();
    let result = std::panic::catch_unwind(|| {
        let _pending = PendingGrant(Some(path.clone()));
        panic!("unconfirmed checkpoint");
    });
    assert!(result.is_err());
    assert_eq!(fs::read_to_string(path.join("checkpoint.txt")).unwrap(), "candidate bytes");
    drop(PendingGrant(Some(path.clone())));
    assert!(!path.exists(), "ordinary rollback removes unpublished files");
}
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "control-project-test-{}-{}-{}",
            std::process::id(),
            nonce(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn source(&self) -> PathBuf {
        let path = self.0.join("source");
        fs::create_dir(&path).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn command(directory: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .arg("-C")
        .arg(directory)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture Git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
#[test]
fn project_request_is_only_intent_and_cannot_replace_an_approved_grant() {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    let value = request(&mut state, &id, "/a/nonexistent/project").unwrap();
    assert_eq!(value["approval_required"], true);
    assert!(state.chats[0].project.as_ref().unwrap().grant.is_none());
    assert!(request(&mut state, &id, "../private").is_err());
    let before = serde_json::to_vec(&state).unwrap();
    assert!(
        crate::session_http::rpc(
            &mut state,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control.project.approve","arguments":{"id":id,"path":"/tmp"}}})
        )["result"]["isError"]
            == true
    );
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    state.chats[0].project.as_mut().unwrap().grant = Some("approved".into());
    request(&mut state, &id, "/another").unwrap();
    assert_eq!(
        state.chats[0].project.as_ref().unwrap().grant.as_deref(),
        Some("approved")
    );
    assert!(state.chats[0].project.as_ref().unwrap().needs_approval());
    let child = state.chats[0].project.as_ref().unwrap().approved().unwrap();
    assert!(!child.pending);
    assert_eq!(child.grant.as_deref(), Some("approved"));
    assert_eq!(child.path, "/a/nonexistent/project");
    assert!(child.mounts.iter().all(|m| m.message_index.is_none()));
}
#[test]
#[ignore = "manual filesystem benchmark: 1 GiB history plus 4000 small files; no containers, models or user projects"]
fn profile_large_project_turn_copies() {
    use std::time::Instant;
    let fixture = Fixture::new();
    let root = fixture.0.join("projects");
    let source = root.join("grants/grant/seed");
    let history = source.join(".control-git/objects/pack");
    fs::create_dir_all(&history).unwrap();
    let mut pack = fs::File::create(history.join("fixture.pack")).unwrap();
    let block = vec![b'p'; 8 * 1024 * 1024];
    for _ in 0..128 {
        pack.write_all(&block).unwrap();
    }
    for n in 0..4000 {
        fs::write(source.join(format!("file-{n:04}")), [b'x'; 1024]).unwrap();
    }
    crate::artifacts::sync_tree(&source).unwrap();
    for trial in 0..3 {
        let directory = fixture.0.join(format!("run-{trial}"));
        let mut attempt = crate::usage::Attempt {
            project: Some("grant".into()),
            ..Default::default()
        };
        let started = Instant::now();
        prepare_in(
            &root,
            &attempt,
            &directory.join("baseline"),
            &directory.join("workspace"),
        )
        .unwrap();
        let prepared = started.elapsed();
        let started = Instant::now();
        publish_with(
            &root,
            &mut attempt,
            &directory.join("workspace"),
            &format!("trial-{trial}"),
            || Ok("preview".into()),
        )
        .unwrap();
        let published = started.elapsed();
        println!(
            "trial={trial} prepare_ms={} checkpoint_ms={}",
            prepared.as_millis(),
            published.as_millis()
        );
        assert_eq!(
            fs::metadata(directory.join("workspace/.control-git/objects/pack/fixture.pack"))
                .unwrap()
                .len(),
            1024 * 1024 * 1024
        );
        assert_eq!(
            fs::read_dir(directory.join("workspace")).unwrap().count(),
            4001
        );
    }
}
#[test]
fn git_worktree_preserves_dirty_files_deletions_history_and_executable_modes() {
    let fixture = Fixture::new();
    let source = fixture.source();
    command(&source, &["init"]);
    fs::write(source.join("main.rs"), "before").unwrap();
    fs::write(source.join("removed.txt"), "remove me").unwrap();
    fs::write(source.join("run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(source.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    command(&source, &["add", "--all"]);
    command(
        &source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-m",
            "original history",
        ],
    );
    fs::write(source.join("main.rs"), "uncommitted").unwrap();
    fs::remove_file(source.join("removed.txt")).unwrap();
    fs::write(source.join("new.txt"), "new file").unwrap();
    fs::write(source.join(".env"), "synthetic excluded value").unwrap();
    fs::create_dir(source.join(".ssh")).unwrap();
    fs::write(source.join(".ssh/id_ed25519"), "synthetic excluded value").unwrap();
    fs::write(source.join(".netrc"), "synthetic excluded value").unwrap();
    fs::create_dir(source.join("target")).unwrap();
    fs::write(source.join("target/build"), "generated").unwrap();
    let imported = fixture.0.join("seed");
    seed(&source, &imported).unwrap();
    assert_eq!(fs::read(imported.join("main.rs")).unwrap(), b"uncommitted");
    assert!(!imported.join("removed.txt").exists());
    assert!(!imported.join(".env").exists());
    assert!(!imported.join(".ssh").exists());
    assert!(!imported.join(".netrc").exists());
    assert!(!imported.join("target").exists());
    assert_eq!(
        fs::metadata(imported.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
    assert!(
        String::from_utf8(command(&imported, &["log", "-1", "--format=%s"]).stdout)
            .unwrap()
            .contains("original history")
    );
    let status = String::from_utf8(command(&imported, &["status", "--porcelain"]).stdout).unwrap();
    assert!(
        status.contains("main.rs") && status.contains("removed.txt") && status.contains("new.txt")
    );
    assert!(command(&imported, &["remote"]).stdout.is_empty());
    assert!(!status.contains(".control-git"));
    let worktrees =
        String::from_utf8(command(&imported, &["worktree", "list", "--porcelain"]).stdout).unwrap();
    assert!(
        worktrees.contains(&format!(
            "worktree {}",
            imported.canonicalize().unwrap().display()
        )),
        "Relative worktree metadata must work on the host: {worktrees}"
    );
    let second = fixture.0.join("independent");
    copy_tree(&imported, &second, true).unwrap();
    fs::write(second.join("main.rs"), "independent edit").unwrap();
    command(&second, &["add", "main.rs"]);
    command(
        &second,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-m",
            "private change",
        ],
    );
    assert_eq!(fs::read(imported.join("main.rs")).unwrap(), b"uncommitted");
    assert_eq!(fs::read(source.join("main.rs")).unwrap(), b"uncommitted");
    assert!(
        String::from_utf8(command(&source, &["log", "-1", "--format=%s"]).stdout)
            .unwrap()
            .contains("original history")
    );
}
#[test]
fn checkpoints_keep_large_files_and_git_separate_from_bounded_previews() {
    let fixture = Fixture::new();
    let source = fixture.source();
    fs::write(source.join("large.bin"), vec![42; 3 * 1024 * 1024]).unwrap();
    seed(&source, &fixture.0.join("seed")).unwrap();
    let root = fixture.0.join("projects");
    let mut attempt = crate::usage::Attempt {
        project: Some("grant".into()),
        ..Default::default()
    };
    publish_with(
        &root,
        &mut attempt,
        &fixture.0.join("seed"),
        "generation-1",
        || Err(fail("viewer limit")),
    )
    .unwrap();
    assert_eq!(attempt.project_checkpoint.as_deref(), Some("generation-1"));
    assert!(attempt.artifact.is_none());
    attempt.project_parent = attempt.project_checkpoint.clone();
    prepare_in(
        &root,
        &attempt,
        &fixture.0.join("baseline"),
        &fixture.0.join("restored"),
    )
    .unwrap();
    assert_eq!(
        fs::metadata(fixture.0.join("restored/large.bin"))
            .unwrap()
            .len(),
        3 * 1024 * 1024
    );
    assert!(fixture.0.join("restored/.git/HEAD").is_file());
    assert!(
        publish_with(
            &root,
            &mut attempt,
            &fixture.0.join("seed"),
            "generation-1",
            || Ok("unused".into())
        )
        .is_err()
    );
}
#[test]
fn import_ignores_links_to_files_directories_missing_paths_and_cycles() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("private.txt"), "synthetic private file").unwrap();
    std::os::unix::fs::symlink(&outside, source.join("escape")).unwrap();
    fs::create_dir(source.join(".clickhouse")).unwrap();
    std::os::unix::fs::symlink(
        "/nix/store/synthetic-missing/bin/clickhouse",
        source.join(".clickhouse/clickhouse"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.join("private.txt"), source.join("linked-file")).unwrap();
    std::os::unix::fs::symlink(&source, source.join("cycle")).unwrap();
    std::os::unix::fs::symlink(&outside, source.join(".git")).unwrap();
    fs::write(source.join("code.txt"), "keep me").unwrap();
    let copied = fixture.0.join("copy");
    copy_tree(&source, &copied, false).unwrap();
    assert_eq!(
        fs::read_to_string(copied.join("code.txt")).unwrap(),
        "keep me"
    );
    for link in ["escape", ".clickhouse/clickhouse", "linked-file", "cycle"] {
        assert!(
            fs::symlink_metadata(copied.join(link)).is_err(),
            "Must skip {link}"
        );
    }
    // This also applies to subsequent checkpoints, including agent-created links.
    copy_tree(&source, &fixture.0.join("checkpoint"), true).unwrap();
    assert!(!fixture.0.join("checkpoint/escape").exists());
    seed(&source, &fixture.0.join("seed")).unwrap();
    command(&fixture.0.join("seed"), &["status", "--porcelain"]);
}
#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "large sparse copy requires a CoW filesystem to stay cheap"
)]
fn project_copy_accepts_files_over_one_gib() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let large = fs::File::create(source.join("large.bin")).unwrap();
    let length = 1024 * 1024 * 1024 + 1;
    large.set_len(length).unwrap();
    let copied = fixture.0.join("large-copy");
    copy_tree(&source, &copied, false).unwrap();
    assert_eq!(
        fs::metadata(copied.join("large.bin")).unwrap().len(),
        length
    );
}
#[test]
fn project_copy_keeps_wide_directories_without_truncation() {
    let fixture = Fixture::new();
    let source = fixture.source();
    for n in 0..2100 {
        fs::write(source.join(format!("file-{n:04}")), "keep").unwrap();
    }
    let copied = fixture.0.join("wide-copy");
    copy_tree(&source, &copied, false).unwrap();
    assert_eq!(
        fs::read_to_string(copied.join("file-2099")).unwrap(),
        "keep"
    );
    assert_eq!(fs::read_dir(copied).unwrap().count(), 2100);
}
#[test]
fn plain_folder_first_turn_and_next_checkpoint_create_fresh_run_directories() {
    let fixture = Fixture::new();
    let source = fixture.source();
    fs::write(source.join("code.txt"), "original").unwrap();
    let root = fixture.0.join("projects");
    let grant = root.join("grants/grant");
    fs::create_dir_all(&grant).unwrap();
    seed(&source, &grant.join("seed")).unwrap();
    let mut attempt = crate::usage::Attempt {
        project: Some("grant".into()),
        ..Default::default()
    };
    let first = fixture.0.join("runs/first");
    prepare_in(
        &root,
        &attempt,
        &first.join("baseline"),
        &first.join("workspace"),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(first.join("workspace/code.txt")).unwrap(),
        "original"
    );
    command(&first.join("workspace"), &["status", "--porcelain"]);
    fs::write(first.join("workspace/code.txt"), "edited").unwrap();
    assert_eq!(
        fs::read_to_string(first.join("baseline/code.txt")).unwrap(),
        "original"
    );
    publish_with(
        &root,
        &mut attempt,
        &first.join("workspace"),
        "first",
        || Ok("fixture-artifact".into()),
    )
    .unwrap();
    attempt.project_parent = attempt.project_checkpoint.clone();
    let second = fixture.0.join("runs/second");
    prepare_in(
        &root,
        &attempt,
        &second.join("baseline"),
        &second.join("workspace"),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(second.join("workspace/code.txt")).unwrap(),
        "edited"
    );
    command(&second.join("workspace"), &["status", "--porcelain"]);
    assert_eq!(
        fs::read_to_string(source.join("code.txt")).unwrap(),
        "original"
    );
    assert!(!source.join(".git").exists());
}
#[test]
fn adding_a_folder_migrates_a_legacy_root_without_reimporting_its_host_files() {
    let fixture = Fixture::new();
    let source = fixture.source();
    fs::write(source.join("code.txt"), "host original").unwrap();
    let root = fixture.0.join("projects");
    let original = root.join("grants/original");
    fs::create_dir_all(&original).unwrap();
    seed(&source, &original.join("seed")).unwrap();
    fs::write(original.join("seed/code.txt"), "private edit").unwrap();
    let second = fixture.0.join("web");
    fs::create_dir(&second).unwrap();
    fs::write(second.join("page.txt"), "page").unwrap();
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    state.chats[0].project = Some(Project {
        path: source.display().to_string(),
        grant: Some("original".into()),
        ..Default::default()
    });
    request(&mut state, &id, second.to_str().unwrap()).unwrap();
    let stage = root.join("grants/combined");
    fs::create_dir_all(&stage).unwrap();
    compose(&root, state.chats[0].project.as_ref().unwrap(), &stage).unwrap();
    assert_eq!(
        fs::read_to_string(stage.join("seed/source/code.txt")).unwrap(),
        "private edit"
    );
    assert_eq!(
        fs::read_to_string(stage.join("seed/web/page.txt")).unwrap(),
        "page"
    );
    assert_eq!(
        fs::read_to_string(source.join("code.txt")).unwrap(),
        "host original"
    );
    for name in ["..", "../escape", ".git", "target", "a/b", "a b"] {
        assert!(request_named(&mut state, &id, second.to_str().unwrap(), Some(name)).is_err());
    }
}
#[test]
fn repeated_folder_requests_keep_message_provenance_and_replace_panel_approval() {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &id, "Discuss first", "test").unwrap();
    crate::chat::send(&mut state, &id, "/open /first", "test").unwrap();
    let first = state.chats[0].project.clone().unwrap();
    state.chats[0].project.as_mut().unwrap().error = Some("Synthetic import failure".into());
    let failed = crate::chat::requests(&state, &id).unwrap();
    assert!(failed.contains("role='alert'>Synthetic import failure"));
    crate::chat::send(&mut state, &id, "/open /second", "test").unwrap();
    let project = state.chats[0].project.as_ref().unwrap();
    assert_ne!(first.request, project.request);
    assert_eq!(project.path, "/second");
    assert!(project.error.is_none());
    assert_eq!(message_index(&state.chats[0]), Some(2));
    let rendered = crate::chat::transcript(&state, &id).unwrap();
    assert!(rendered.contains("/open</span> /second"));
    assert!(!rendered.contains("Trust folder and open"));
    let panel = crate::chat::requests(&state, &id).unwrap();
    assert_eq!(panel.matches("Trust folder and open").count(), 1);
    assert!(!panel.contains(&format!("value='{}'", first.request)));
    assert!(rendered.contains("Folder request replaced by a later request."));
    assert!(!rendered.contains(&format!("value='{}'", first.request)));
    assert!(!rendered.contains("Synthetic import failure"));
    // Old state still binds a request to its matching command, not later chat.
    state.chats[0].project.as_mut().unwrap().message_index = None;
    state.chats[0].messages.push(crate::chat::Message { speaker: Default::default(),
        text: "Later message".into(),
        run_id: None,
        reply: "Later response".into(),
    });
    assert_eq!(message_index(&state.chats[0]), Some(2));
    let restored: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(message_index(&restored.chats[0]), Some(2));
}
#[test]
#[ignore = "requires isolated CONTROL_STATE_DIR and CONTROL_CONFIG_DIR under /private/tmp/control-project-qa; no credentials or models"]
fn live_folder_approval_errors_and_multiple_named_worktrees() {
    let state_dir = crate::config::state_directory().unwrap();
    assert!(state_dir.parent().is_some_and(|p| {
        p.parent() == Some(Path::new("/private/tmp"))
            && p.file_name()
                .is_some_and(|s| s.to_string_lossy().starts_with("control-project-qa."))
    }));
    crate::config::initialize(include_bytes!("../../assets/discord-access.example.json")).unwrap();
    crate::storage::initialize().unwrap();
    let fixture = Fixture::new();
    let first = fixture.0.join("one/api");
    let second = fixture.0.join("two/api");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    fs::write(first.join("code.txt"), "first").unwrap();
    fs::write(second.join("code.txt"), "second").unwrap();
    std::os::unix::fs::symlink("/nix/store/synthetic-missing", first.join("binary")).unwrap();
    command(&second, &["init"]);
    command(&second, &["add", "--all"]);
    command(
        &second,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-m",
            "baseline",
        ],
    );
    let mut initial = State::default();
    let id = crate::chat::create(&mut initial);
    let shared = Arc::new(Mutex::new(initial));
    let open = |path: &str, name: Option<&str>| {
        let mut state = shared.lock().unwrap();
        let text = match name {
            Some(name) => format!("/open --as {name} {path}"),
            None => format!("/open {path}"),
        };
        crate::chat::send(&mut state, &id, &text, "test").unwrap();
    };
    let approve = || {
        let request = shared.lock().unwrap().chats[0]
            .project
            .as_ref()
            .unwrap()
            .request
            .clone();
        let mut response = Vec::new();
        assert!(http(
            &mut response,
            &shared,
            "POST",
            &format!("/dm/{id}/project"),
            "",
            format!("request={request}&approve=yes").as_bytes()
        ));
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 303"), "{response}");
        assert!(response.contains(&format!("location: /dm/{id}/project?importing={request}\r\n")));
        wait_for_import(&id);
    };
    open(&fixture.0.join("missing").display().to_string(), None);
    approve();
    {
        let state = shared.lock().unwrap();
        let project = state.chats[0].project.as_ref().unwrap();
        assert!(project.error.is_some());
        assert!(project.grant.is_none());
        assert!(
            crate::chat::requests(&state, &id)
                .unwrap()
                .contains("role='alert'")
        );
    }
    open(&first.display().to_string(), None);
    approve();
    let original = shared.lock().unwrap().chats[0].project.clone().unwrap();
    assert!(!original.pending);
    assert_eq!(original.mounts[0].name, "api");
    let mut attempt = crate::usage::Attempt {
        project: original.grant.clone(),
        ..Default::default()
    };
    let first_run = state_dir.join("runs/first");
    prepare(
        &attempt,
        &first_run.join("baseline"),
        &first_run.join("workspace"),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(first_run.join("workspace/api/code.txt")).unwrap(),
        "first"
    );
    assert!(!first_run.join("workspace/api/binary").exists());
    fs::write(first_run.join("workspace/api/code.txt"), "edited first").unwrap();
    publish_with(
        &root().unwrap(),
        &mut attempt,
        &first_run.join("workspace"),
        "first-checkpoint",
        || Ok("fixture-artifact".into()),
    )
    .unwrap();
    shared.lock().unwrap().chats[0]
        .project
        .as_mut()
        .unwrap()
        .head = attempt.project_checkpoint.clone();
    open(&second.display().to_string(), None);
    approve();
    {
        let state = shared.lock().unwrap();
        let project = state.chats[0].project.as_ref().unwrap();
        assert_eq!(project.grant, original.grant);
        assert_eq!(project.head, attempt.project_checkpoint);
        assert!(project.pending);
        assert!(project.error.as_deref().unwrap().contains("already open"));
    }
    open(&second.display().to_string(), Some("api2"));
    approve();
    let updated = shared.lock().unwrap().chats[0].project.clone().unwrap();
    assert!(!updated.pending);
    assert!(updated.error.is_none());
    assert_eq!(updated.mounts.len(), 2);
    assert_ne!(updated.grant, original.grant);
    let attempt = crate::usage::Attempt {
        project: updated.grant,
        ..Default::default()
    };
    let second_run = state_dir.join("runs/second");
    prepare(
        &attempt,
        &second_run.join("baseline"),
        &second_run.join("workspace"),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(second_run.join("workspace/api/code.txt")).unwrap(),
        "edited first"
    );
    assert_eq!(
        fs::read_to_string(second_run.join("workspace/api2/code.txt")).unwrap(),
        "second"
    );
    command(
        &second_run.join("workspace/api"),
        &["status", "--porcelain"],
    );
    command(
        &second_run.join("workspace/api2"),
        &["status", "--porcelain"],
    );
    assert_eq!(fs::read_to_string(first.join("code.txt")).unwrap(), "first");
    assert!(!first.join(".git").exists());
    let state = shared.lock().unwrap();
    let transcript = crate::chat::transcript(&state, &id).unwrap();
    assert!(transcript.contains("Opened /workspace/api"));
    assert!(!transcript.contains("Trust folder and open"));
    let saved: State =
        crate::config::read_state_in(&state_dir, isize::MAX as usize).unwrap().unwrap();
    assert_eq!(saved.chats[0].project.as_ref().unwrap().mounts.len(), 2);
}
fn wait_for_import(id: &str) {
    let started = std::time::Instant::now();
    while importing(id).is_some() {
        assert!(started.elapsed() < std::time::Duration::from_secs(60), "import did not finish");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
/// Holds approval attachment until the test opens it, standing in for a slow copy.
static GATE: (Mutex<bool>, std::sync::Condvar) = (Mutex::new(false), std::sync::Condvar::new());
fn gated_save(_: &State) -> io::Result<()> {
    let (open, changed) = &GATE;
    let mut open = open.lock().unwrap();
    while !*open { open = changed.wait(open).unwrap(); }
    Ok(())
}
fn open_gate() {
    *GATE.0.lock().unwrap() = true;
    GATE.1.notify_all();
}
fn no_save(_: &State) -> io::Result<()> { Ok(()) }
fn exchange(shared: &Arc<Mutex<State>>, method: &str, path: &str, query: &str, body: &str, save: Save) -> String {
    let mut response = Vec::new();
    assert!(http_in(&mut response, shared, method, path, query, body.as_bytes(), save));
    String::from_utf8(response).unwrap()
}
fn pending_request(shared: &Arc<Mutex<State>>) -> String {
    shared.lock().unwrap().chats[0].project.as_ref().unwrap().request.clone()
}
#[test]
fn approval_responds_at_once_imports_in_the_background_and_returns_to_the_chat() {
    let fixture = Fixture::new();
    let first = fixture.0.join("mc263-client");
    let second = fixture.0.join("mc-pi-skills");
    for folder in [&first, &second] {
        fs::create_dir(folder).unwrap();
        fs::write(folder.join("README.md"), "tool").unwrap();
    }
    let mut initial = State::default();
    let id = crate::chat::create(&mut initial);
    crate::chat::send(&mut initial, &id, &format!("/open {}", first.display()), "test").unwrap();
    let shared = Arc::new(Mutex::new(initial));
    let page = format!("/dm/{id}/project");
    let request = pending_request(&shared);
    // The attachment is held open, yet approval answers immediately.
    let response = exchange(&shared, "POST", &page, "", &format!("request={request}&approve=yes"), gated_save);
    assert!(response.starts_with("HTTP/1.1 303"), "{response}");
    assert!(response.contains(&format!("location: /dm/{id}/project?importing={request}\r\n")), "{response}");
    assert_eq!(importing(&id).unwrap().request, request);
    // A repeated click neither fails nor starts another copy.
    let repeated = exchange(&shared, "POST", &page, "", &format!("request={request}&approve=yes"), gated_save);
    assert!(repeated.starts_with("HTTP/1.1 303"), "{repeated}");
    assert_eq!(IMPORTS.lock().unwrap().iter().filter(|import| import.chat == id).count(), 1);
    let progress = exchange(&shared, "GET", &page, &format!("importing={request}"), "", no_save);
    assert!(progress.starts_with("HTTP/1.1 200"), "{progress}");
    assert!(progress.contains("Importing folder…") && progress.contains("role='status'"), "{progress}");
    assert!(progress.contains(&format!("http-equiv='refresh' content='1;url=/dm/{id}/project?importing={request}'")));
    assert!(!progress.contains("Trust folder and open"), "no second approval while importing");
    {
        let mut state = shared.lock().unwrap();
        assert!(crate::chat::transcript(&state, &id).unwrap().contains("Importing folder…"));
        assert_eq!(crate::chat::detail(&state, &id).unwrap()["project_importing"], true);
        assert!(crate::chat::send(&mut state, &id, "hello", "test").unwrap_err().contains("still importing"));
        assert!(state.chats[0].project.as_ref().unwrap().needs_approval());
    }
    open_gate();
    wait_for_import(&id);
    let done = exchange(&shared, "GET", &page, &format!("importing={request}"), "", no_save);
    assert!(done.starts_with("HTTP/1.1 303"), "{done}");
    assert!(done.contains(&format!("location: /dm/{id}\r\n")), "{done}");
    let grant = {
        let state = shared.lock().unwrap();
        let project = state.chats[0].project.as_ref().unwrap();
        assert!(!project.needs_approval() && project.error.is_none());
        assert_eq!(project.mounts.len(), 1);
        project.grant.clone().unwrap()
    };
    // Adding a folder to the open project follows the same path.
    crate::chat::send(&mut shared.lock().unwrap(), &id, &format!("/open {}", second.display()), "test").unwrap();
    let request = pending_request(&shared);
    let response = exchange(&shared, "POST", &page, "", &format!("request={request}&approve=yes"), gated_save);
    assert!(response.contains(&format!("location: /dm/{id}/project?importing={request}\r\n")), "{response}");
    wait_for_import(&id);
    let done = exchange(&shared, "GET", &page, &format!("importing={request}"), "", no_save);
    assert!(done.contains(&format!("location: /dm/{id}\r\n")), "{done}");
    let state = shared.lock().unwrap();
    let project = state.chats[0].project.as_ref().unwrap();
    assert_eq!(project.mounts.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["mc263-client", "mc-pi-skills"]);
    assert_ne!(project.grant.as_deref(), Some(grant.as_str()));
    let seed = root().unwrap().join("grants").join(project.grant.as_ref().unwrap()).join("seed");
    assert_eq!(fs::read_to_string(seed.join("mc-pi-skills/README.md")).unwrap(), "tool");
    assert!(seed.join("mc263-client/README.md").exists());
}
/// The in-chat request panel shows a copy's entry count, which no checkpoint
/// records. Without its own hint the page stayed on the first count until a
/// manual reload (regressed when approval moved to the background, f5458c6).
#[test]
fn copied_entries_hint_open_pages_at_a_bounded_rate() {
    let _reader = crate::live_updates::fixture_subscriber().expect("reader slot");
    let entries = Arc::new(AtomicUsize::new(0));
    IMPORT_PROGRESS.with(|progress| *progress.borrow_mut() = Some(entries.clone()));
    IMPORT_HINTED.with(|hinted| hinted.set(None));
    let before = crate::live_updates::generation();
    copied_entry().unwrap();
    let hinted = IMPORT_HINTED.with(|hinted| hinted.get()).expect("the first entry is shown at once");
    assert!(crate::live_updates::generation() > before, "connected pages are told to re-read");
    copied_entry().unwrap();
    assert_eq!(IMPORT_HINTED.with(|hinted| hinted.get()), Some(hinted), "a burst of entries shares one hint");
    assert_eq!(entries.load(Ordering::Relaxed), 2);
    IMPORT_PROGRESS.with(|progress| *progress.borrow_mut() = None);
    // Ending an import (here, its registry entry) hints readers again, so a
    // failure saved while it still showed as running is not left stale.
    let before = crate::live_updates::generation();
    drop(Registered { chat: "c-none".into(), request: "p-0".into() });
    assert!(crate::live_updates::generation() > before);
}
#[test]
fn in_chat_folder_decisions_answer_like_port_decisions() {
    let fixture = Fixture::new();
    let folder = fixture.source();
    fs::write(folder.join("code.txt"), "code").unwrap();
    let mut initial = State::default();
    let id = crate::chat::create(&mut initial);
    crate::chat::send(&mut initial, &id, &format!("/open {}", folder.display()), "test").unwrap();
    let shared = Arc::new(Mutex::new(initial));
    let page = format!("/dm/{id}/project");
    {
        let panel = crate::chat::requests(&shared.lock().unwrap(), &id).unwrap();
        assert!(panel.contains(&format!("<form class='decision-form' method='post' action='{page}'")), "{panel}");
        assert!(panel.contains("Trust folder and open</button><p role='alert' hidden></p></form>"));
    }
    // A stale request is refused in the form, not as an error page.
    let stale = exchange(&shared, "POST", &page, "", "request=p-0&approve=yes&response=json", no_save);
    assert!(stale.starts_with("HTTP/1.1 400") && stale.contains("application/json") && stale.contains("request changed"), "{stale}");
    let request = pending_request(&shared);
    let accepted = exchange(&shared, "POST", &page, "", &format!("request={request}&approve=yes&response=json"), no_save);
    assert!(accepted.starts_with("HTTP/1.1 200") && accepted.ends_with("{\"ok\":true}"), "{accepted}");
    wait_for_import(&id);
    assert!(!shared.lock().unwrap().chats[0].project.as_ref().unwrap().needs_approval());
    // An import that fails is reported to the form that approved it.
    crate::chat::send(&mut shared.lock().unwrap(), &id, &format!("/open {}", fixture.0.join("missing").display()), "test").unwrap();
    let request = pending_request(&shared);
    let accepted = exchange(&shared, "POST", &page, "", &format!("request={request}&approve=yes&response=json"), no_save);
    if accepted.starts_with("HTTP/1.1 200") { wait_for_import(&id); } else { assert!(accepted.contains("\"error\""), "{accepted}"); }
    assert!(shared.lock().unwrap().chats[0].project.as_ref().unwrap().error.is_some());
}
#[test]
fn background_import_errors_appear_beside_the_request_for_retry() {
    let fixture = Fixture::new();
    let mut initial = State::default();
    let id = crate::chat::create(&mut initial);
    let missing = fixture.0.join("missing");
    crate::chat::send(&mut initial, &id, &format!("/open {}", missing.display()), "test").unwrap();
    let shared = Arc::new(Mutex::new(initial));
    let page = format!("/dm/{id}/project");
    let request = pending_request(&shared);
    let response = exchange(&shared, "POST", &page, "", &format!("request={request}&approve=yes"), no_save);
    assert!(response.contains(&format!("location: /dm/{id}/project?importing={request}\r\n")), "{response}");
    wait_for_import(&id);
    let shown = exchange(&shared, "GET", &page, &format!("importing={request}"), "", no_save);
    assert!(shown.starts_with("HTTP/1.1 200"), "{shown}");
    assert!(shown.contains("role='alert'"), "{shown}");
    assert!(shown.contains("Trust folder and open"), "the request stays available to retry");
    assert!(!shown.contains("http-equiv='refresh'"));
    {
        let state = shared.lock().unwrap();
        let project = state.chats[0].project.as_ref().unwrap();
        assert!(project.error.is_some() && project.grant.is_none());
        assert!(crate::chat::transcript(&state, &id).unwrap().contains("Folder request · see Requests"));
    }
    // Approving a replaced request is refused and grants nothing.
    let stale = exchange(&shared, "POST", &page, "", "request=p-0&approve=yes", no_save);
    assert!(stale.starts_with("HTTP/1.1 400") && stale.contains("request changed"), "{stale}");
    assert!(importing(&id).is_none());
    fs::create_dir(&missing).unwrap();
    fs::write(missing.join("code.txt"), "now present").unwrap();
    let retry = exchange(&shared, "POST", &page, "", &format!("request={request}&approve=yes"), no_save);
    assert!(retry.starts_with("HTTP/1.1 303"), "{retry}");
    wait_for_import(&id);
    let project = shared.lock().unwrap().chats[0].project.clone().unwrap();
    assert!(!project.needs_approval() && project.error.is_none());
}
#[test]
#[ignore = "requires CONTROL_QA_IMAGE pinned Nix image and local engine; no credentials or model calls"]
fn live_container_git_commits_and_restores_an_independent_project_worktree() {
    let fixture = Fixture::new();
    let source = fixture.source();
    command(&source, &["init"]);
    fs::write(source.join("code.txt"), "original").unwrap();
    command(&source, &["add", "--all"]);
    command(
        &source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-m",
            "original",
        ],
    );
    let workspace = fixture.0.join("workspace");
    seed(&source, &workspace).unwrap();
    let image = std::env::var("CONTROL_QA_IMAGE").expect("Pinned local Nix image");
    assert!(image.starts_with("sha256:"));
    let run = |root: &Path, script: &str| {
        let mount = format!(
            "type=bind,src={},dst=/workspace",
            root.canonicalize().unwrap().display()
        );

        crate::engine::query(&[
            "run",
            "--rm",
            "--pull=never",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--user",
            &format!("{}:{}", unsafe { libc::getuid() }, unsafe {
                libc::getgid()
            }),
            "--tmpfs",
            "/tmp:rw,nosuid,size=64m",
            "--env",
            "HOME=/tmp",
            "--mount",
            &mount,
            "--workdir",
            "/workspace",
            &image,
            "bash",
            "-c",
            script,
        ])
        .unwrap()
    };
    assert!(run(&workspace,"set -eu; test ! -e /home/agent/.codex/auth.json; test ! -S /var/run/docker.sock; git status --porcelain; printf edited > code.txt; git add --all; git -c user.name=Fixture -c user.email=fixture@localhost commit -m edited; git worktree list --porcelain").contains("worktree /workspace\n"));
    let copy = fixture.0.join("restored");
    copy_tree(&workspace, &copy, true).unwrap();
    assert_eq!(run(&copy, "git log -1 --format=%s").trim(), "edited");
    assert_eq!(fs::read(source.join("code.txt")).unwrap(), b"original");
    assert!(!run(&copy, "git status --porcelain").contains(".control-git"));
}
