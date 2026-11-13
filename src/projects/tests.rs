
use super::*;
use std::os::unix::fs::PermissionsExt;
struct Fixture(PathBuf);
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
    state.chats[0].messages.push(crate::chat::Message {
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
            &format!("/chat/{id}/project"),
            format!("request={request}&approve=yes").as_bytes()
        ));
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 303"), "{response}");
        assert!(response.contains(&format!("Location: /chat/{id}\r\n")));
    };
    open(&fixture.0.join("missing").display().to_string(), None);
    approve();
    {
        let state = shared.lock().unwrap();
        let project = state.chats[0].project.as_ref().unwrap();
        assert!(project.error.is_some());
        assert!(project.grant.is_none());
        assert!(
            crate::chat::transcript(&state, &id)
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
        serde_json::from_slice(&fs::read(state_dir.join("state.json")).unwrap()).unwrap();
    assert_eq!(saved.chats[0].project.as_ref().unwrap().mounts.len(), 2);
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
