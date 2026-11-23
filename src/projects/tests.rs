use super::*;
use crate::runtime::RunStatus;
use std::os::unix::fs::PermissionsExt;
struct Fixture(PathBuf);
/// Agent requests are never probed, so fixtures may name absent folders.
const AGENT: Requester<'static> = Requester::Agent { run: None };

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
    assert_eq!(
        fs::read_to_string(path.join("checkpoint.txt")).unwrap(),
        "candidate bytes"
    );
    drop(PendingGrant(Some(path.clone())));
    assert!(!path.exists(), "ordinary rollback removes unpublished files");
}
impl Fixture {
    fn new() -> Self {
        let path = crate::test_support::scratch("control-project-test");
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
        // Fixture commits must not leave detached maintenance in the source
        // either: the import copies and clones it right after.
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "maintenance.auto=false",
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
/// Flaked approvals on macOS CI: `git commit` in `seed` detached `git
/// maintenance run --auto`, which created and removed `objects/maintenance.lock`
/// while the import was still syncing and moving the new repository (ENOENT).
/// Git traces the spawn before the commit returns, so this needs no timing.
#[test]
fn import_git_starts_no_background_maintenance() {
    let fixture = Fixture::new();
    let repository = fixture.source();
    fs::write(repository.join("code.txt"), "code").unwrap();
    let trace = fixture.0.join("trace.txt");
    let git = |args: &[&str]| {
        let args = [&["-C", repository.to_str().unwrap()], args].concat();
        let args = args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>();
        git_output(&args, &[("GIT_TRACE", trace.as_os_str())]).unwrap();
    };
    git(&["init"]);
    git(&["add", "--all"]);
    git(&[
        "-c",
        "user.name=Control",
        "-c",
        "user.email=control@localhost",
        "commit",
        "-m",
        "baseline",
    ]);
    let trace = fs::read_to_string(trace).unwrap();
    assert!(trace.contains("commit"), "{trace}");
    assert!(!trace.contains("maintenance"), "{trace}");
}
#[test]
fn project_request_is_only_intent_and_cannot_replace_an_approved_grant() {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    let value = request(&mut state, &id, "/a/nonexistent/project", AGENT).unwrap();
    assert_eq!(value["approval_required"], true);
    assert!(state.chats[0].project.as_ref().unwrap().grant.is_none());
    assert!(request(&mut state, &id, "../private", AGENT).is_err());
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
    request(&mut state, &id, "/another", AGENT).unwrap();
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
        assert_eq!(fs::read_dir(directory.join("workspace")).unwrap().count(), 4001);
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
        fs::metadata(imported.join("run.sh")).unwrap().permissions().mode() & 0o111,
        0o111
    );
    assert!(
        String::from_utf8(command(&imported, &["log", "-1", "--format=%s"]).stdout)
            .unwrap()
            .contains("original history")
    );
    let status = String::from_utf8(command(&imported, &["status", "--porcelain"]).stdout).unwrap();
    assert!(status.contains("main.rs") && status.contains("removed.txt") && status.contains("new.txt"));
    assert!(command(&imported, &["remote"]).stdout.is_empty());
    assert!(!status.contains(".control-git"));
    let worktrees = String::from_utf8(command(&imported, &["worktree", "list", "--porcelain"]).stdout).unwrap();
    assert!(
        worktrees.contains(&format!("worktree {}", imported.canonicalize().unwrap().display())),
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
    publish_with(&root, &mut attempt, &fixture.0.join("seed"), "generation-1", || {
        Err(io::Error::other("viewer limit"))
    })
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
        fs::metadata(fixture.0.join("restored/large.bin")).unwrap().len(),
        3 * 1024 * 1024
    );
    assert!(fixture.0.join("restored/.git/HEAD").is_file());
    assert!(
        publish_with(&root, &mut attempt, &fixture.0.join("seed"), "generation-1", || Ok(
            "unused".into()
        ))
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
    assert_eq!(fs::read_to_string(copied.join("code.txt")).unwrap(), "keep me");
    for link in ["escape", ".clickhouse/clickhouse", "linked-file", "cycle"] {
        assert!(fs::symlink_metadata(copied.join(link)).is_err(), "Must skip {link}");
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
    assert_eq!(fs::metadata(copied.join("large.bin")).unwrap().len(), length);
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
    assert_eq!(fs::read_to_string(copied.join("file-2099")).unwrap(), "keep");
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
    prepare_in(&root, &attempt, &first.join("baseline"), &first.join("workspace")).unwrap();
    assert_eq!(
        fs::read_to_string(first.join("workspace/code.txt")).unwrap(),
        "original"
    );
    command(&first.join("workspace"), &["status", "--porcelain"]);
    fs::write(first.join("workspace/code.txt"), "edited").unwrap();
    assert_eq!(fs::read_to_string(first.join("baseline/code.txt")).unwrap(), "original");
    publish_with(&root, &mut attempt, &first.join("workspace"), "first", || {
        Ok("fixture-artifact".into())
    })
    .unwrap();
    attempt.project_parent = attempt.project_checkpoint.clone();
    let second = fixture.0.join("runs/second");
    prepare_in(&root, &attempt, &second.join("baseline"), &second.join("workspace")).unwrap();
    assert_eq!(fs::read_to_string(second.join("workspace/code.txt")).unwrap(), "edited");
    command(&second.join("workspace"), &["status", "--porcelain"]);
    assert_eq!(fs::read_to_string(source.join("code.txt")).unwrap(), "original");
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
    request(&mut state, &id, second.to_str().unwrap(), Requester::Operator).unwrap();
    let stage = root.join("grants/combined");
    fs::create_dir_all(&stage).unwrap();
    compose(&root, state.chats[0].project.as_ref().unwrap(), &stage).unwrap();
    assert_eq!(
        fs::read_to_string(stage.join("seed/source/code.txt")).unwrap(),
        "private edit"
    );
    assert_eq!(fs::read_to_string(stage.join("seed/web/page.txt")).unwrap(), "page");
    assert_eq!(fs::read_to_string(source.join("code.txt")).unwrap(), "host original");
    for name in ["..", "../escape", ".git", "target", "a/b", "a b"] {
        assert!(
            request_named(
                &mut state,
                &id,
                second.to_str().unwrap(),
                Some(name),
                Requester::Operator
            )
            .is_err()
        );
    }
}
#[test]
fn repeated_folder_requests_keep_message_provenance_and_replace_panel_approval() {
    let fixture = Fixture::new();
    let (first_path, second_path) = (fixture.0.join("first"), fixture.0.join("second"));
    fs::create_dir(&first_path).unwrap();
    fs::create_dir(&second_path).unwrap();
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &id, "Discuss first", "test").unwrap();
    crate::chat::send(&mut state, &id, &format!("/open {}", first_path.display()), "test").unwrap();
    let first = state.chats[0].project.clone().unwrap();
    state.chats[0].project.as_mut().unwrap().error = Some("Synthetic import failure".into());
    let failed = crate::chat::requests(&state, &id).unwrap();
    assert!(failed.contains("role='alert'>Synthetic import failure"));
    crate::chat::send(&mut state, &id, &format!("/open {}", second_path.display()), "test").unwrap();
    let project = state.chats[0].project.as_ref().unwrap();
    assert_ne!(first.request, project.request);
    assert_eq!(project.path, second_path.display().to_string());
    assert!(project.error.is_none());
    assert_eq!(message_index(&state.chats[0]), Some(2));
    let rendered = crate::chat::transcript(&state, &id).unwrap().render();
    assert!(rendered.contains(&format!("/open</span> {}", second_path.display())));
    assert!(!rendered.contains("Trust folder and open"));
    let panel = crate::chat::requests(&state, &id).unwrap();
    assert_eq!(panel.matches("Trust folder and open").count(), 1);
    assert!(!panel.contains(&format!("value='{}'", first.request)));
    assert!(rendered.contains("Folder request replaced by a later request."));
    assert!(!rendered.contains(&format!("value='{}'", first.request)));
    assert!(!rendered.contains("Synthetic import failure"));
    // Old state still binds a request to its matching command, not later chat.
    state.chats[0].project.as_mut().unwrap().message_index = None;
    state.chats[0].messages.push(crate::chat::Message {
        speaker: Default::default(),
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
        assert!(crate::chat::requests(&state, &id).unwrap().contains("role='alert'"));
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
    prepare(&attempt, &first_run.join("baseline"), &first_run.join("workspace")).unwrap();
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
    shared.lock().unwrap().chats[0].project.as_mut().unwrap().head = attempt.project_checkpoint.clone();
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
    prepare(&attempt, &second_run.join("baseline"), &second_run.join("workspace")).unwrap();
    assert_eq!(
        fs::read_to_string(second_run.join("workspace/api/code.txt")).unwrap(),
        "edited first"
    );
    assert_eq!(
        fs::read_to_string(second_run.join("workspace/api2/code.txt")).unwrap(),
        "second"
    );
    command(&second_run.join("workspace/api"), &["status", "--porcelain"]);
    command(&second_run.join("workspace/api2"), &["status", "--porcelain"]);
    assert_eq!(fs::read_to_string(first.join("code.txt")).unwrap(), "first");
    assert!(!first.join(".git").exists());
    let state = shared.lock().unwrap();
    let transcript = crate::chat::transcript(&state, &id).unwrap().render();
    assert!(transcript.contains("Opened /workspace/api"));
    assert!(!transcript.contains("Trust folder and open"));
    let saved: State = crate::config::read_state_in(&state_dir, isize::MAX as usize)
        .unwrap()
        .unwrap();
    assert_eq!(saved.chats[0].project.as_ref().unwrap().mounts.len(), 2);
}
fn wait_for_import(id: &str) {
    let started = std::time::Instant::now();
    while importing(id).is_some() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "import did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
/// Chats whose imports wait after copying and before attaching, standing in for
/// a slow copy. Keyed by chat so parallel tests never hold each other's imports,
/// and waiting outside the state lock so the test can still read the chat.
static HELD: (Mutex<Vec<String>>, std::sync::Condvar) = (Mutex::new(Vec::new()), std::sync::Condvar::new());
pub(super) fn hold(chat: &str) {
    let (held, changed) = &HELD;
    let mut held = held.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    while held.iter().any(|c| c == chat) {
        held = changed.wait(held).unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}
/// Holds `chat`'s imports until dropped, also when the test fails.
struct Held(String);
impl Held {
    fn new(chat: &str) -> Self {
        HELD.0.lock().unwrap().push(chat.into());
        Self(chat.into())
    }
}
impl Drop for Held {
    fn drop(&mut self) {
        HELD.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|c| *c != self.0);
        HELD.1.notify_all();
    }
}
fn no_save(_: &State) -> io::Result<()> {
    Ok(())
}
fn exchange(shared: &Arc<Mutex<State>>, method: &str, path: &str, query: &str, body: &str, save: Save) -> String {
    let mut response = Vec::new();
    assert!(http_in(
        &mut response,
        shared,
        method,
        path,
        query,
        body.as_bytes(),
        save
    ));
    String::from_utf8(response).unwrap()
}
fn pending_request(shared: &Arc<Mutex<State>>) -> String {
    shared.lock().unwrap().chats[0]
        .project
        .as_ref()
        .unwrap()
        .request
        .clone()
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
    // The import is held before attaching, yet approval answers immediately.
    let held = Held::new(&id);
    let response = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&approve=yes"),
        no_save,
    );
    assert!(response.starts_with("HTTP/1.1 303"), "{response}");
    assert!(
        response.contains(&format!("location: /dm/{id}/project?importing={request}\r\n")),
        "{response}"
    );
    assert_eq!(importing(&id).unwrap().request, request);
    // A repeated click neither fails nor starts another copy.
    let repeated = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&approve=yes"),
        no_save,
    );
    assert!(repeated.starts_with("HTTP/1.1 303"), "{repeated}");
    assert_eq!(
        IMPORTS
            .lock()
            .unwrap()
            .iter()
            .filter(|import| import.chat == id)
            .count(),
        1
    );
    let progress = exchange(&shared, "GET", &page, &format!("importing={request}"), "", no_save);
    assert!(progress.starts_with("HTTP/1.1 200"), "{progress}");
    assert!(
        progress.contains("Importing folder…") && progress.contains("role='status'"),
        "{progress}"
    );
    assert!(progress.contains(&format!(
        "http-equiv='refresh' content='1;url=/dm/{id}/project?importing={request}'"
    )));
    assert!(
        !progress.contains("Trust folder and open"),
        "no second approval while importing"
    );
    {
        let mut state = shared.lock().unwrap();
        assert!(
            crate::chat::transcript(&state, &id)
                .unwrap()
                .render()
                .contains("Importing folder…")
        );
        assert_eq!(crate::chat::detail(&state, &id).unwrap()["project_importing"], true);
        assert!(
            crate::chat::send(&mut state, &id, "hello", "test")
                .unwrap_err()
                .contains("still importing")
        );
        assert!(state.chats[0].project.as_ref().unwrap().needs_approval());
    }
    drop(held);
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
    crate::chat::send(
        &mut shared.lock().unwrap(),
        &id,
        &format!("/open {}", second.display()),
        "test",
    )
    .unwrap();
    let request = pending_request(&shared);
    let response = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&approve=yes"),
        no_save,
    );
    assert!(
        response.contains(&format!("location: /dm/{id}/project?importing={request}\r\n")),
        "{response}"
    );
    wait_for_import(&id);
    let done = exchange(&shared, "GET", &page, &format!("importing={request}"), "", no_save);
    assert!(done.contains(&format!("location: /dm/{id}\r\n")), "{done}");
    let state = shared.lock().unwrap();
    let project = state.chats[0].project.as_ref().unwrap();
    assert_eq!(
        project.mounts.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        ["mc263-client", "mc-pi-skills"]
    );
    assert_ne!(project.grant.as_deref(), Some(grant.as_str()));
    let seed = root()
        .unwrap()
        .join("grants")
        .join(project.grant.as_ref().unwrap())
        .join("seed");
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
    let hinted = IMPORT_HINTED
        .with(|hinted| hinted.get())
        .expect("the first entry is shown at once");
    assert!(
        crate::live_updates::generation() > before,
        "connected pages are told to re-read"
    );
    copied_entry().unwrap();
    assert_eq!(
        IMPORT_HINTED.with(|hinted| hinted.get()),
        Some(hinted),
        "a burst of entries shares one hint"
    );
    assert_eq!(entries.load(Ordering::Relaxed), 2);
    IMPORT_PROGRESS.with(|progress| *progress.borrow_mut() = None);
    // Ending an import (here, its registry entry) hints readers again, so a
    // failure saved while it still showed as running is not left stale.
    let before = crate::live_updates::generation();
    drop(Registered {
        chat: "c-none".into(),
        request: "p-0".into(),
    });
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
        assert!(
            panel.contains(&format!("<form class='decision-form' method='post' action='{page}'")),
            "{panel}"
        );
        assert!(panel.contains("Trust folder and open</button><button class='secondary' name='dismiss' value='yes'>Dismiss</button><p role='alert' hidden></p></form>"));
    }
    // A stale request is refused in the form, not as an error page.
    let stale = exchange(
        &shared,
        "POST",
        &page,
        "",
        "request=p-0&approve=yes&response=json",
        no_save,
    );
    assert!(
        stale.starts_with("HTTP/1.1 400") && stale.contains("application/json") && stale.contains("request changed"),
        "{stale}"
    );
    let request = pending_request(&shared);
    let accepted = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&approve=yes&response=json"),
        no_save,
    );
    assert!(
        accepted.starts_with("HTTP/1.1 200") && accepted.ends_with("{\"ok\":true}"),
        "{accepted}"
    );
    wait_for_import(&id);
    assert!(
        !shared.lock().unwrap().chats[0]
            .project
            .as_ref()
            .unwrap()
            .needs_approval()
    );
    // An import that fails is reported to the form that approved it.
    super::request(
        &mut shared.lock().unwrap(),
        &id,
        fixture.0.join("missing").to_str().unwrap(),
        AGENT,
    )
    .unwrap();
    let request = pending_request(&shared);
    let accepted = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&approve=yes&response=json"),
        no_save,
    );
    if accepted.starts_with("HTTP/1.1 200") {
        wait_for_import(&id);
    } else {
        assert!(accepted.contains("\"error\""), "{accepted}");
    }
    assert!(
        shared.lock().unwrap().chats[0]
            .project
            .as_ref()
            .unwrap()
            .error
            .is_some()
    );
}
#[test]
fn background_import_errors_appear_beside_the_request_for_retry() {
    let fixture = Fixture::new();
    let mut initial = State::default();
    let id = crate::chat::create(&mut initial);
    let missing = fixture.0.join("missing");
    // The folder exists when requested and is gone by approval.
    fs::create_dir(&missing).unwrap();
    crate::chat::send(&mut initial, &id, &format!("/open {}", missing.display()), "test").unwrap();
    fs::remove_dir(&missing).unwrap();
    let shared = Arc::new(Mutex::new(initial));
    let page = format!("/dm/{id}/project");
    let request = pending_request(&shared);
    let response = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&approve=yes"),
        no_save,
    );
    assert!(
        response.contains(&format!("location: /dm/{id}/project?importing={request}\r\n")),
        "{response}"
    );
    wait_for_import(&id);
    let shown = exchange(&shared, "GET", &page, &format!("importing={request}"), "", no_save);
    assert!(shown.starts_with("HTTP/1.1 200"), "{shown}");
    assert!(shown.contains("role='alert'"), "{shown}");
    assert!(
        shown.contains("Trust folder and open"),
        "the request stays available to retry"
    );
    assert!(!shown.contains("http-equiv='refresh'"));
    {
        let state = shared.lock().unwrap();
        let project = state.chats[0].project.as_ref().unwrap();
        assert!(project.error.is_some() && project.grant.is_none());
        assert!(
            crate::chat::transcript(&state, &id)
                .unwrap()
                .render()
                .contains("Folder request · see Requests")
        );
    }
    // Approving a replaced request is refused and grants nothing.
    let stale = exchange(&shared, "POST", &page, "", "request=p-0&approve=yes", no_save);
    assert!(
        stale.starts_with("HTTP/1.1 400") && stale.contains("request changed"),
        "{stale}"
    );
    assert!(importing(&id).is_none());
    fs::create_dir(&missing).unwrap();
    fs::write(missing.join("code.txt"), "now present").unwrap();
    let retry = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&approve=yes"),
        no_save,
    );
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
            &format!("{}:{}", unsafe { libc::getuid() }, unsafe { libc::getgid() }),
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
/// `/open /open /x` once made a pending request for a path that never existed,
/// with no way to reject it.
#[test]
fn operator_open_checks_the_folder_but_agent_requests_are_never_probed() {
    let fixture = Fixture::new();
    let folder = fixture.source();
    let file = fixture.0.join("notes.txt");
    fs::write(&file, "text").unwrap();
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    assert!(
        crate::chat::send(&mut state, &id, &format!("/open {}", file.display()), "test")
            .unwrap_err()
            .contains("not a folder")
    );
    assert!(
        crate::chat::send(
            &mut state,
            &id,
            &format!("/open --as other {}/missing", folder.display()),
            "test"
        )
        .is_err()
    );
    assert!(
        state.chats[0].project.is_none() && state.chats[0].messages.is_empty(),
        "nothing is requested or recorded"
    );
    // A doubled command reads as a list; its refused part requests nothing.
    let typo = format!("/open /open {}", folder.display());
    crate::chat::send(&mut state, &id, &typo, "test").unwrap();
    assert!(state.chats[0].messages[0].reply.contains("- /open: No readable folder"));
    assert!(state.chats[0].project.is_none(), "a typo never becomes a request");
    crate::chat::send(&mut state, &id, &format!("/open {}", folder.display()), "test").unwrap();
    assert!(state.chats[0].project.as_ref().unwrap().origin.is_none());
    // An agent learns nothing about the host: a folder, a file and a missing
    // path give the same answer, and the requesting turn is recorded.
    let answers = [folder.clone(), file, fixture.0.join("missing")].map(|path| {
        let mut value = request_named(
            &mut state,
            &id,
            path.to_str().unwrap(),
            Some("asked"),
            Requester::Agent { run: Some("r-asking") },
        )
        .unwrap();
        assert_eq!(
            state.chats[0].project.as_ref().unwrap().origin.as_deref(),
            Some("r-asking")
        );
        for varying in ["request", "path"] {
            value.as_object_mut().unwrap().remove(varying);
        }
        value
    });
    assert!(answers.iter().all(|answer| *answer == answers[0]), "{answers:?}");
}
fn approved(path: &str) -> Project {
    let name = Path::new(path).file_name().unwrap().to_str().unwrap();
    Project {
        request: "p-approved".into(),
        path: path.into(),
        name: name.into(),
        grant: Some("p-grant".into()),
        head: Some("c-before".into()),
        named: true,
        mounts: vec![Mount {
            name: name.into(),
            path: path.into(),
            message_index: Some(0),
        }],
        ..Project::default()
    }
}
fn snapshot(project: &Option<Project>) -> Value {
    serde_json::to_value(project).unwrap()
}
#[test]
fn dismissal_restores_the_previous_folders_and_ignores_stale_or_repeated_requests() {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    state.chats[0].project = Some(approved("/node/api"));
    request(&mut state, &id, "/node/replaced", AGENT).unwrap();
    // Replacing a pending request still restores the approved project.
    request_named(&mut state, &id, "/open /node/typo", Some("typo"), AGENT).unwrap();
    let pending = state.chats[0].project.clone().unwrap();
    assert!(pending.needs_approval() && pending.mounts.len() == 1);
    // Approved folders keep working while the request waits; a checkpoint
    // published meanwhile survives dismissal.
    state.chats[0].project.as_mut().unwrap().head = Some("c-after".into());
    let mut expected = approved("/node/api");
    expected.head = Some("c-after".into());
    // A stale page names an older request: nothing changes.
    let before = serde_json::to_vec(&state).unwrap();
    assert!(dismiss(&mut state, &id, "p-0").unwrap().is_none());
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    assert_eq!(
        dismiss(&mut state, &id, &pending.request).unwrap(),
        Some(crate::approvals::Outcome::Noted)
    );
    assert_eq!(snapshot(&state.chats[0].project), snapshot(&Some(expected)));
    let notice = state.chats[0].messages.last().unwrap();
    assert!(
        notice.text.starts_with("Folder decisions\n/open /node/typo (")
            && notice.text.contains("dismissed by the operator"),
        "{}",
        notice.text
    );
    assert!(notice.is_notice(None));
    // A repeated dismissal, or one for the approved request, is a no-op.
    let after = serde_json::to_vec(&state).unwrap();
    assert!(dismiss(&mut state, &id, &pending.request).unwrap().is_none());
    assert!(dismiss(&mut state, &id, "p-approved").unwrap().is_none());
    assert_eq!(serde_json::to_vec(&state).unwrap(), after);
    // With no earlier folder, the chat returns to having none.
    let fresh = crate::chat::create(&mut state);
    request(&mut state, &fresh, "/node/first", AGENT).unwrap();
    let index = state.chats.iter().position(|c| c.id == fresh).unwrap();
    let first = state.chats[index].project.as_ref().unwrap().request.clone();
    dismiss(&mut state, &fresh, &first).unwrap().unwrap();
    assert!(state.chats[index].project.is_none());
    // Requests saved before predecessors were recorded keep approved folders.
    let mut legacy = approved("/node/api");
    (legacy.pending, legacy.request, legacy.path, legacy.name) =
        (true, "p-legacy".into(), "/node/new".into(), "new".into());
    state.chats[index].project = Some(legacy);
    dismiss(&mut state, &fresh, "p-legacy").unwrap().unwrap();
    let kept = state.chats[index].project.as_ref().unwrap();
    assert!(
        !kept.needs_approval()
            && kept.path == "/node/api"
            && kept.name == "api"
            && kept.grant.as_deref() == Some("p-grant")
    );
}
#[test]
fn dismissal_reaches_the_requesting_agent_like_a_port_decision() {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &id, "Review the shared library", "test").unwrap();
    let origin = state.runtime.runs[0].id().as_str().to_owned();
    let run = Some(origin.as_str());
    request(&mut state, &id, "/node/library", Requester::Agent { run }).unwrap();
    // A busy chat only notes it; the agent reads it on its next turn.
    let busy = state.chats[0].project.as_ref().unwrap().request.clone();
    assert_eq!(
        dismiss(&mut state, &id, &busy).unwrap(),
        Some(crate::approvals::Outcome::Noted)
    );
    assert_eq!(state.runtime.runs.len(), 1);
    state.runtime.runs[0].status = RunStatus::Completed;
    request(&mut state, &id, "/node/library", Requester::Agent { run }).unwrap();
    let idle = state.chats[0].project.as_ref().unwrap().request.clone();
    let Some(crate::approvals::Outcome::Resumed(resumed)) = dismiss(&mut state, &id, &idle).unwrap() else {
        panic!("resumes the finished turn")
    };
    let continuation = state.runtime.runs.last().unwrap();
    assert_eq!(
        (continuation.id().as_str(), continuation.kind),
        (resumed.as_str(), crate::runtime::RunKind::FolderDecision)
    );
    assert!(
        continuation
            .objective
            .starts_with("Operator folder decisions:\n/node/library ("),
        "{}",
        *continuation.objective
    );
    assert!(
        continuation
            .objective
            .contains("Original task:\nReview the shared library")
    );
}
#[test]
fn dismissal_from_the_page_names_its_request_and_never_breaks_the_page() {
    let fixture = Fixture::new();
    let folder = fixture.source();
    let mut initial = State::default();
    let id = crate::chat::create(&mut initial);
    crate::chat::send(&mut initial, &id, &format!("/open {}", folder.display()), "test").unwrap();
    let shared = Arc::new(Mutex::new(initial));
    let page = format!("/dm/{id}/project");
    let request = pending_request(&shared);
    let stale = exchange(
        &shared,
        "POST",
        &page,
        "",
        "request=p-0&dismiss=yes&response=json",
        no_save,
    );
    assert!(
        stale.starts_with("HTTP/1.1 200") && stale.contains("already dismissed, replaced or approved"),
        "{stale}"
    );
    assert!(
        shared.lock().unwrap().chats[0]
            .project
            .as_ref()
            .is_some_and(|p| p.request == request)
    );
    // A dismissal that cannot be saved leaves the request in place.
    let unsaved = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&dismiss=yes&response=json"),
        failed_save,
    );
    assert!(
        unsaved.starts_with("HTTP/1.1 400") && unsaved.contains("request is unchanged"),
        "{unsaved}"
    );
    assert!(
        shared.lock().unwrap().chats[0]
            .project
            .as_ref()
            .is_some_and(|p| p.request == request && p.needs_approval())
    );
    assert_eq!(shared.lock().unwrap().chats[0].messages.len(), 1);
    let dismissed = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&dismiss=yes&response=json"),
        no_save,
    );
    assert!(
        dismissed.starts_with("HTTP/1.1 200") && dismissed.contains(&format!("\"message\":\"{DISMISSED}\"")),
        "{dismissed}"
    );
    {
        let state = shared.lock().unwrap();
        assert!(state.chats[0].project.is_none());
        assert_eq!(state.chats[0].messages[0].reply, DISMISSED);
        let transcript = crate::chat::transcript(&state, &id).unwrap().render();
        assert!(
            transcript.contains(DISMISSED) && !transcript.contains("replaced by a later request"),
            "{transcript}"
        );
        assert!(
            !crate::chat::requests(&state, &id)
                .unwrap()
                .contains("Trust folder and open")
        );
    }
    // The approval page's own (non-script) form gets a page, not an error.
    let again = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&dismiss=yes"),
        no_save,
    );
    assert!(
        again.starts_with("HTTP/1.1 200") && again.contains("role='status'>That folder request was already dismissed"),
        "{again}"
    );
    // An approval for a dismissed request grants nothing.
    let late = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&approve=yes&response=json"),
        no_save,
    );
    assert!(late.starts_with("HTTP/1.1 400"), "{late}");
    assert!(importing(&id).is_none() && shared.lock().unwrap().chats[0].project.is_none());
}
fn failed_save(_: &State) -> io::Result<()> {
    Err(io::Error::other("disk full"))
}
/// The copy cannot be cancelled: once approved, a request imports to the end.
#[test]
fn a_request_that_is_importing_cannot_be_dismissed() {
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    request(&mut state, &id, "/node/large", AGENT).unwrap();
    let request = state.chats[0].project.as_ref().unwrap().request.clone();
    IMPORTS.lock().unwrap().push(Import {
        chat: id.clone(),
        request: request.clone(),
        started: std::time::Instant::now(),
        entries: Arc::default(),
    });
    let registered = Registered {
        chat: id.clone(),
        request: request.clone(),
    };
    let before = serde_json::to_vec(&state).unwrap();
    assert!(dismiss(&mut state, &id, &request).unwrap_err().contains("importing"));
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    drop(registered);
    assert!(dismiss(&mut state, &id, &request).unwrap().is_some());
}
#[test]
fn approving_twice_starts_one_import_and_keeps_its_grant() {
    let fixture = Fixture::new();
    let folder = fixture.source();
    fs::write(folder.join("code.txt"), "code").unwrap();
    let mut initial = State::default();
    let id = crate::chat::create(&mut initial);
    crate::chat::send(&mut initial, &id, &format!("/open {}", folder.display()), "test").unwrap();
    let shared = Arc::new(Mutex::new(initial));
    let page = format!("/dm/{id}/project");
    let request = pending_request(&shared);
    let first = exchange(
        &shared,
        "POST",
        &page,
        "",
        &format!("request={request}&approve=yes&response=json"),
        no_save,
    );
    assert!(first.ends_with("{\"ok\":true}"), "{first}");
    wait_for_import(&id);
    let opened = shared.lock().unwrap().chats[0].project.clone().unwrap();
    assert!(!opened.needs_approval() && opened.previous.is_none() && opened.origin.is_none());
    // Once attached, the approval is not replayed: refused, with nothing copied.
    for body in [
        format!("request={request}&approve=yes&response=json"),
        format!("request={request}&approve=yes"),
    ] {
        let repeated = exchange(&shared, "POST", &page, "", &body, no_save);
        assert!(
            repeated.starts_with("HTTP/1.1 400") && repeated.contains("request changed"),
            "{repeated}"
        );
        assert!(importing(&id).is_none(), "no second copy starts");
    }
    assert_eq!(
        snapshot(&shared.lock().unwrap().chats[0].project),
        snapshot(&Some(opened))
    );
}
/// Only a complete copy is ever published under `grants/`, and only while the
/// checkpoint that attaches it is written.
#[test]
fn failed_or_interrupted_imports_publish_nothing() {
    let fixture = Fixture::new();
    let root = fixture.0.join("projects");
    let folder = fixture.source();
    fs::write(folder.join("a.txt"), "copied first").unwrap();
    fs::write(folder.join("z.txt"), "unreadable").unwrap();
    fs::set_permissions(folder.join("z.txt"), fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::File::open(folder.join("z.txt")).is_ok();
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    request(&mut state, &id, folder.to_str().unwrap(), AGENT).unwrap();
    let project = state.chats[0].project.clone().unwrap();
    let shared = Arc::new(Mutex::new(state));
    let published = || {
        fs::read_dir(root.join("grants"))
            .map(|entries| {
                entries
                    .map(|e| e.unwrap().file_name().into_string().unwrap())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    // Superusers read anything; the unreadable-entry case needs a normal user.
    if !readable {
        assert!(
            import_in(&root, &shared, &id, &project.request, project.clone(), no_save)
                .unwrap_err()
                .to_string()
                .contains("unreadable")
        );
        assert!(published().is_empty(), "{:?}", published());
    }
    fs::set_permissions(folder.join("z.txt"), fs::Permissions::from_mode(0o600)).unwrap();
    // A checkpoint that cannot be written attaches nothing and keeps no copy.
    assert!(import_in(&root, &shared, &id, &project.request, project.clone(), failed_save).is_err());
    assert!(published().is_empty(), "{:?}", published());
    assert!(
        shared.lock().unwrap().chats[0]
            .project
            .as_ref()
            .unwrap()
            .needs_approval()
    );
    // A process that stopped mid-copy left only a staging directory, which the
    // next start removes; published grants stay.
    fs::create_dir_all(root.join("grants/.pending-p-1/combined")).unwrap();
    fs::write(root.join("grants/.pending-p-1/combined/half.txt"), "partial").unwrap();
    import_in(&root, &shared, &id, &project.request, project.clone(), no_save).unwrap();
    let grant = shared.lock().unwrap().chats[0]
        .project
        .as_ref()
        .unwrap()
        .grant
        .clone()
        .unwrap();
    assert_eq!(
        fs::read_to_string(root.join("grants").join(&grant).join("seed/source/z.txt")).unwrap(),
        "unreadable"
    );
    discard_interrupted_in(&root).unwrap();
    assert_eq!(published(), [grant]);
    discard_interrupted_in(&fixture.0.join("absent")).unwrap();
}

#[test]
fn open_lists_split_on_commas_lines_or_a_space_before_the_next_path() {
    let home = Some("/home/me");
    assert_eq!(paths(" /a/api /b/web", home), ["/a/api", "/b/web"]);
    assert_eq!(
        paths("/Users/me/My Project", home),
        ["/Users/me/My Project"],
        "one path with spaces still opens alone"
    );
    assert_eq!(
        paths("/Users/me/My Project /srv/web", home),
        ["/Users/me/My Project", "/srv/web"]
    );
    assert_eq!(paths("/a, /b c ,/d", home), ["/a", "/b c", "/d"]);
    assert_eq!(
        paths("/a /x\n/b /y\n\n", home),
        ["/a /x", "/b /y"],
        "each line is one path"
    );
    assert_eq!(
        paths("\"/data/a /b\" '/c d' /e", home),
        ["/data/a /b", "/c d", "/e"],
        "quotes keep a space before a /"
    );
    assert_eq!(paths("~ ~/code/api /x", home), ["/home/me", "/home/me/code/api", "/x"]);
    assert_eq!(
        paths("~/code", None),
        ["~/code"],
        "no home, no expansion: refused later as relative"
    );
    assert_eq!(paths("relative/path", home), ["relative/path"]);
    assert!(paths("  ", home).is_empty());
}

#[test]
fn operator_open_list_reports_each_path_and_requests_the_valid_folders_together() {
    let fixture = Fixture::new();
    let api = fixture.0.join("api");
    let web = fixture.0.join("my web");
    fs::create_dir_all(&api).unwrap();
    fs::create_dir_all(&web).unwrap();
    fs::write(fixture.0.join("notes.txt"), "text").unwrap();
    let mut state = State::default();
    let id = crate::chat::create(&mut state);
    let base = fixture.0.display();
    // One per line, a space inside a name, a duplicate name, a file and a
    // missing folder: each path gets its own result, and any refusal requests
    // nothing, so the corrected list is sent again whole.
    crate::chat::send(
        &mut state,
        &id,
        &format!("/open {base}/api\n{base}/my web\n{base}/api\n{base}/notes.txt\n{base}/gone"),
        "test",
    )
    .unwrap();
    let lines = state.chats[0].messages[0].reply.lines().collect::<Vec<_>>();
    assert!(lines[0].starts_with("No folder was requested"), "{lines:?}");
    assert!(lines[1].ends_with("/api → /workspace/api"), "{lines:?}");
    assert!(
        lines[2].contains("my web: Folder names must use letters"),
        "names come from the folder; {lines:?}"
    );
    assert!(lines[3].contains("already used"), "{lines:?}");
    assert!(lines[4].contains("notes.txt: That path is not a folder"), "{lines:?}");
    assert!(lines[5].contains("gone: No readable folder"), "{lines:?}");
    assert!(state.chats[0].project.is_none());
    // Valid folders become one request with a mount each.
    let other = fixture.0.join("other");
    fs::create_dir_all(&other).unwrap();
    crate::chat::send(&mut state, &id, &format!("/open {base}/api {base}/other"), "test").unwrap();
    let index = state.chats[0].messages.len() - 1;
    let lines = state.chats[0].messages[index].reply.lines().collect::<Vec<_>>();
    assert!(
        lines[0].starts_with("Review and approve folder access") && lines[2].ends_with("/other → /workspace/other"),
        "{lines:?}"
    );
    let project = state.chats[0].project.clone().unwrap();
    assert!(project.pending && project.name == "api" && project.message_index == Some(index));
    assert_eq!(
        project
            .also
            .iter()
            .map(|m| (m.name.as_str(), m.message_index))
            .collect::<Vec<_>>(),
        [("other", Some(index))]
    );
    let panel = panel(&state.chats, &id);
    assert!(
        panel.contains("Trust these 2 folders")
            && panel.contains("<code>/workspace/other</code>")
            && !panel.contains("Change folder"),
        "{panel}"
    );
    // `--as` names exactly one folder.
    assert!(
        crate::chat::send(
            &mut state,
            &id,
            &format!("/open --as x {base}/api {base}/other"),
            "test"
        )
        .is_err()
    );

    // One approval copies both folders, each to its own mount.
    let root = fixture.0.join("projects");
    let shared = Arc::new(Mutex::new(state));
    fs::write(api.join("a.txt"), "a").unwrap();
    fs::write(other.join("b.txt"), "b").unwrap();
    import_in(&root, &shared, &id, &project.request, project.clone(), no_save).unwrap();
    let state = shared.lock().unwrap();
    let opened = state.chats[0].project.as_ref().unwrap();
    assert!(!opened.pending && opened.also.is_empty());
    assert_eq!(
        opened.mounts.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        ["api", "other"]
    );
    let seed = root.join("grants").join(opened.grant.as_ref().unwrap()).join("seed");
    assert_eq!(
        (
            fs::read_to_string(seed.join("api/a.txt")).unwrap(),
            fs::read_to_string(seed.join("other/b.txt")).unwrap()
        ),
        ("a".into(), "b".into())
    );
}
