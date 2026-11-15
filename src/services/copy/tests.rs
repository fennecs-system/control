use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

struct Scratch(PathBuf);
impl Scratch {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "control-copy-{label}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn write(&self, path: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
/// Extracts with the host's tar, as the service would, into a fresh directory.
fn extract(archive: &[u8]) -> Scratch {
    let destination = Scratch::new("extract");
    let mut child = std::process::Command::new("tar")
        .args(["-x", "-f", "-", "-C"])
        .arg(&destination.0)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(archive).unwrap();
    assert!(child.wait().unwrap().success());
    destination
}
fn transfer(from: &str, to: &str) -> Transfer {
    Transfer::new(from, to).unwrap()
}

#[test]
fn paths_stay_relative_plain_and_publishable() {
    assert_eq!(transfer("/workspace/mc-client/", "tools/mc"), Transfer { from: "mc-client".into(), to: "tools/mc".into() });
    // Generated names may be destinations; only the source policy excludes them.
    assert_eq!(transfer("src/main.rs", "/workspace/app/node_modules/x").to, "app/node_modules/x");
    for from in ["", "/", "/workspace/", "/etc/passwd", "../outside", "a/../b", "a/./b", "a//b", ".", "..", ".git/config", "src/.env", "a/target/x", ".control-mailbox", "bad\nname"] {
        assert!(Transfer::new(from, "x").is_err(), "{from:?}");
    }
    for to in ["", "/", "/etc", "../x", "a/../b", "a/./b", "a//b", "tab\there"] {
        assert!(Transfer::new("x", to).is_err(), "{to:?}");
    }
    let deep = vec!["d"; MAX_DEPTH + 1].join("/");
    assert!(Transfer::new(&deep, "x").is_err());
    assert!(Transfer::new("x", &"y".repeat(1025)).is_err());
}

#[test]
fn trees_stream_regular_files_and_never_follow_links_or_special_files() {
    let root = Scratch::new("source");
    let outside = Scratch::new("outside");
    outside.write("secret", b"host secret");
    root.write("tool/README", b"hello");
    let run = root.write("tool/bin/run", b"#!/bin/sh\necho run\n");
    fs::set_permissions(&run, fs::Permissions::from_mode(0o750)).unwrap();
    let long = format!("tool/{}/{}/data.bin", "a".repeat(60), "b".repeat(60));
    root.write(&long, &[0, 1, 2, 255]);
    root.write("tool/.env", b"TOKEN=1");
    root.write("tool/target/debug/big", b"generated");
    root.write("tool/.git/config", b"[core]");
    symlink(outside.0.join("secret"), root.0.join("tool/link")).unwrap();
    symlink(&outside.0, root.0.join("tool/linked-dir")).unwrap();
    symlink("missing", root.0.join("tool/dangling")).unwrap();
    let fifo = CString::new(root.0.join("tool/fifo").as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    symlink(root.0.join("tool"), root.0.join("alias")).unwrap();

    let scanned = scan(&root.0, "tool", Bounds::DEFAULT).unwrap();
    let mut archive = Vec::new();
    let streamed = stream(&root.0, &transfer("tool", "tools/mc"), Bounds::DEFAULT, (1000, 1000), &mut archive).unwrap();
    assert_eq!(scanned, streamed);
    // tool, bin, the two long directories; README, run, data.bin.
    assert_eq!((streamed.files, streamed.directories, streamed.bytes), (3, 4, 5 + 19 + 4));
    assert_eq!(streamed.skipped, 7);
    for reason in [".env (excluded name)", ".git (excluded name)", "target (excluded name)", "link (symlink or special file)", "linked-dir (symlink or special file)", "dangling (symlink or special file)", "fifo (symlink or special file)"] {
        assert!(streamed.examples.iter().any(|example| example == reason), "{reason}: {:?}", streamed.examples);
    }
    assert_eq!(archive.len() % 512, 0);
    let extracted = extract(&archive);
    let base = extracted.0.join("tools/mc");
    assert_eq!(fs::read(base.join("README")).unwrap(), b"hello");
    assert_eq!(fs::read(base.join(long.strip_prefix("tool/").unwrap())).unwrap(), [0, 1, 2, 255]);
    assert_ne!(fs::metadata(base.join("bin/run")).unwrap().permissions().mode() & 0o111, 0);
    assert_eq!(fs::metadata(base.join("README")).unwrap().permissions().mode() & 0o111, 0);
    for absent in [".env", "target", ".git", "link", "linked-dir", "dangling", "fifo"] {
        assert!(fs::symlink_metadata(base.join(absent)).is_err(), "{absent}");
    }

    // A single file lands exactly at the destination path.
    let mut archive = Vec::new();
    let single = stream(&root.0, &transfer("tool/README", "notes/readme.txt"), Bounds::DEFAULT, (1000, 1000), &mut archive).unwrap();
    assert_eq!((single.files, single.directories), (1, 0));
    assert_eq!(fs::read(extract(&archive).0.join("notes/readme.txt")).unwrap(), b"hello");

    // The source and its parents are never resolved through links.
    for (from, message) in [
        ("tool/link", "symlinks are not followed"),
        ("tool/fifo", "regular file or directory"),
        ("alias/README", "not a link"),
        ("alias", "symlinks are not followed"),
        ("tool/linked-dir/secret", "not a link"),
        ("tool/missing", "not found"),
    ] {
        let error = scan(&root.0, from, Bounds::DEFAULT).unwrap_err().to_string();
        assert!(error.contains(message), "{from}: {error}");
    }
    // Nor is the workspace root itself.
    let linked_root = outside.0.join("workspace-link");
    symlink(&root.0, &linked_root).unwrap();
    assert!(scan(&linked_root, "tool", Bounds::DEFAULT).unwrap_err().to_string().contains("unavailable"));
}

#[test]
fn byte_and_entry_bounds_are_enforced_before_and_during_streaming() {
    let root = Scratch::new("bounds");
    root.write("tree/a", &[1; 10]);
    root.write("tree/b", &[2; 10]);
    root.write("tree/sub/c", &[3; 10]);
    // Exactly at the bounds: tree, a, b, sub, c and 30 bytes.
    let exact = Bounds { bytes: 30, entries: 5 };
    assert_eq!(scan(&root.0, "tree", exact).unwrap().bytes, 30);
    let error = scan(&root.0, "tree", Bounds { bytes: 29, ..exact }).unwrap_err().to_string();
    assert!(error.contains("sub/c") && error.contains("29-byte limit"), "{error}");
    let error = scan(&root.0, "tree", Bounds { entries: 4, ..exact }).unwrap_err().to_string();
    assert!(error.contains("4-entry limit"), "{error}");
    // A failed stream never writes the end-of-archive marker.
    let mut archive = Vec::new();
    assert!(stream(&root.0, &transfer("tree", "x"), Bounds { bytes: 25, ..exact }, (1, 1), &mut archive).is_err());
    assert!(!archive.ends_with(&[0; 1024]));
    // A single file larger than the bound is refused too.
    assert!(scan(&root.0, "tree/a", Bounds { bytes: 9, entries: 1 }).is_err());
    // Directory listings are bounded even when every extra entry is skipped.
    for n in 0..8 {
        symlink("x", root.0.join(format!("tree/link-{n}"))).unwrap();
    }
    assert!(scan(&root.0, "tree", exact).unwrap_err().to_string().contains("entry limit"));
}

#[test]
fn bounds_come_from_node_settings_and_workspace_capacity() {
    let settings = |values: &'static [(&'static str, &'static str)]| move |name: &str| values.iter().find(|(key, _)| *key == name).map(|(_, value)| (*value).to_owned());
    assert_eq!(Bounds::from_settings(|_| None), Bounds::DEFAULT);
    assert_eq!(
        Bounds::from_settings(settings(&[("service-copy-max-mib", "64\n"), ("service-copy-max-entries", "500")])),
        Bounds { bytes: 64 * MIB, entries: 500 }
    );
    assert_eq!(Bounds::from_settings(settings(&[("service-copy-max-mib", "0"), ("service-copy-max-entries", "lots")])), Bounds::DEFAULT);
    assert_eq!(Bounds::from_settings(settings(&[("service-copy-max-mib", "8192")])), Bounds::DEFAULT);
    let (state, ..) = fixture();
    let mut service = state.lock().unwrap().services[0].clone();
    service.storage = Storage::Workspace { limit_mb: std::num::NonZeroU32::new(1) };
    assert_eq!(Bounds::DEFAULT.for_service(&service), Bounds::DEFAULT, "the volume is not under the layer quota");
    service.storage = Storage::Disk { limit_mb: std::num::NonZeroU32::new(16) };
    assert_eq!(Bounds::DEFAULT.for_service(&service).bytes, 16 * MIB);
    service.storage = Storage::Memory;
    assert_eq!(Bounds::DEFAULT.for_service(&service).bytes, service.profile.service_workspace_mb() * MIB);
}

#[test]
fn pax_records_count_their_own_length() {
    for value in ["x".to_owned(), "y".repeat(93), "z".repeat(94), "w".repeat(995)] {
        let record = pax_record("path", &value);
        let (length, rest) = record.split_once(' ').unwrap();
        assert_eq!(length.parse::<usize>().unwrap(), record.len());
        assert_eq!(rest, format!("path={value}\n"));
    }
    let mut field = [0u8; 8];
    octal(&mut field, u64::MAX);
    assert_eq!(&field, b"7777777\0");
}

fn fixture() -> (Arc<Mutex<State>>, String, NodeId, String) {
    let (mut state, run, _, time) = super::super::tests::fixture();
    super::super::tests::admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    state.services[0].resource.phase = Phase::Running("b".repeat(64).try_into().unwrap());
    let id = state.services[0].resource.id.as_str().to_owned();
    (Arc::new(Mutex::new(state)), run, "node-a".to_owned().try_into().unwrap(), id)
}
fn at<'a>(run: &'a str, generation: &'a str, nonce: &'a str) -> AttemptRef<'a> {
    AttemptRef { run, generation, nonce }
}
fn input(service: &str, from: &str, to: &str) -> Input {
    Input { service_id: service.into(), from: from.into(), to: to.into(), timeout_seconds: 60 }
}

#[test]
fn admission_is_scoped_bounded_durable_and_idempotent() {
    let (shared, run, node, id) = fixture();
    let root = Scratch::new("admission");
    root.write("tool/main.rs", b"fn main() {}");
    let workspace = |generation: &str| {
        assert_eq!(generation, "attempt-a");
        Ok(root.0.clone())
    };
    // A failed checkpoint admits nothing.
    assert!(request_with(&shared, &at(&run, "attempt-a", "copy-a"), &node, input(&id, "tool", "tool"), workspace, |_| Err(fail("disk full"))).is_err());
    assert!(shared.lock().unwrap().services[0].executions.is_empty());
    let admitted = request_with(&shared, &at(&run, "attempt-a", "copy-a"), &node, input(&id, "/workspace/tool", "tool"), workspace, |_| Ok(())).unwrap();
    assert_eq!(admitted["status"], "requested");
    assert_eq!((admitted["copy"]["files"].as_u64(), admitted["copy"]["directories"].as_u64(), admitted["copy"]["bytes"].as_u64()), (Some(1), Some(1), Some(12)));
    assert_eq!(admitted["copy"]["limits"]["bytes"], Bounds::DEFAULT.bytes);
    let text = serde_json::to_string(&admitted).unwrap();
    assert!(!text.contains(root.0.to_str().unwrap()), "results never expose host paths");
    {
        let state = shared.lock().unwrap();
        let execution = &state.services[0].executions[0];
        let (command, display, copy) = execution.request();
        assert!(command.is_empty(), "an older Control must not launch anything");
        assert_eq!(copy, Some(&transfer("tool", "tool")));
        assert_eq!(display, "copy /workspace/tool -> service /workspace/tool");
        let stored = serde_json::to_string(&*state).unwrap();
        assert!(!stored.contains(root.0.to_str().unwrap()), "durable state never stores host paths");
    }
    // A retry returns the recorded request without reading or saving again,
    // even after the source changed.
    fs::remove_dir_all(root.0.join("tool")).unwrap();
    let retried = request_with(&shared, &at(&run, "attempt-a", "copy-a"), &node, input(&id, "tool", "tool"), |_| panic!("Retries never rescan"), |_| panic!("Retries never save")).unwrap();
    assert_eq!(retried["execution_id"], admitted["execution_id"]);
    let error = request_with(&shared, &at(&run, "attempt-a", "copy-a"), &node, input(&id, "tool", "elsewhere"), |_| panic!("no scan"), |_| panic!("no save")).unwrap_err();
    assert!(error.to_string().contains("reused with different arguments"));
    // The same identity cannot turn into a shell command either.
    let mut state = shared.lock().unwrap();
    let command = commands::Work { service_id: id.clone(), digest: commands::hash(b"other"), command: "true".into(), display: "true".into(), timeout_seconds: 60, copy: None };
    assert!(commands::admit_work(&mut state, &run, "attempt-a", "copy-a", &node, command, |_| panic!("no save")).is_err());
    // One lane: a new copy waits for the active one.
    drop(state);
    root.write("tool/main.rs", b"fn main() {}");
    let busy = request_with(&shared, &at(&run, "attempt-a", "copy-b"), &node, input(&id, "tool", "tool"), workspace, |_| panic!("no save")).unwrap_err();
    assert!(busy.to_string().contains("active command"), "{busy}");
}

#[test]
fn admission_rejects_foreign_services_judges_stale_attempts_and_oversized_sources() {
    fn never(_: &str) -> io::Result<PathBuf> {
        panic!("Unauthorized requests never read the workspace")
    }
    let refuse = |shared: &Arc<Mutex<State>>, run: &str, generation: &str, node: &NodeId, id: &str| {
        request_with(shared, &at(run, generation, "copy-x"), node, input(id, "tool", "tool"), never, |_| panic!("no save")).unwrap_err().to_string()
    };
    // Another chat's service.
    let (shared, run, node, id) = fixture();
    let other = crate::chat::create(&mut shared.lock().unwrap());
    shared.lock().unwrap().runtime.runs[0].chat_id = Some(other);
    assert!(refuse(&shared, &run, "attempt-a", &node, &id).contains("not owned by this chat"));
    // Another node's identity.
    let (shared, run, _, id) = fixture();
    assert!(refuse(&shared, &run, "attempt-a", &"node-b".to_owned().try_into().unwrap(), &id).contains("not owned"));
    // A judge, even of the owning chat.
    let (shared, run, node, id) = fixture();
    shared.lock().unwrap().runtime.runs[0].persona = Some(crate::delegation::Persona { role: crate::delegation::Role::Judge, root: "root".into(), depth: 1, parent: None });
    assert!(refuse(&shared, &run, "attempt-a", &node, &id).contains("read-only attempt"));
    // An earlier attempt of the same run.
    let (shared, run, node, id) = fixture();
    assert!(refuse(&shared, &run, "attempt-old", &node, &id).contains("inactive"));
    // A source over the memory-backed workspace size fails before admission.
    let (shared, run, node, id) = fixture();
    shared.lock().unwrap().services[0].storage = Storage::Memory;
    let root = Scratch::new("oversized");
    let file = fs::OpenOptions::new().write(true).open(root.write("tool/huge", b"")).unwrap();
    file.set_len(shared.lock().unwrap().services[0].profile.service_workspace_mb() * MIB + 1).unwrap();
    let error = request_with(&shared, &at(&run, "attempt-a", "copy-y"), &node, input(&id, "tool", "tool"), |_| Ok(root.0.clone()), |_| panic!("no save")).unwrap_err();
    assert!(error.to_string().contains("limit"), "{error}");
    assert!(shared.lock().unwrap().services[0].executions.is_empty());
    // A copy whose attempt ended before dispatch launches nothing.
    let (shared, run, node, id) = fixture();
    let root = Scratch::new("ended");
    root.write("tool/x", b"x");
    request_with(&shared, &at(&run, "attempt-a", "copy-z"), &node, input(&id, "tool", "tool"), |_| Ok(root.0.clone()), |_| Ok(())).unwrap();
    shared.lock().unwrap().runtime.runs[0].status = "completed".into();
    let service = shared.lock().unwrap().services[0].binding_snapshot();
    let error = Job::prepare(&shared, &service, &run, "attempt-a", &transfer("tool", "tool")).err().unwrap();
    assert!(error.to_string().contains("attempt ended"));
}

/// Drives the real dispatcher and engine: no model calls or credentials.
/// Run with an isolated `CONTROL_STATE_DIR` under a temporary directory; the
/// test writes that state and the attempt workspace there.
#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE, an isolated temporary CONTROL_STATE_DIR and a local Linux engine; no credentials/models"]
fn live_copy_streams_a_tree_into_a_real_service_as_its_user() {
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let state_dir = PathBuf::from(std::env::var("CONTROL_STATE_DIR").expect("set an isolated CONTROL_STATE_DIR"));
    assert!(
        state_dir.starts_with("/private/tmp") || state_dir.starts_with("/tmp") || state_dir.starts_with(std::env::temp_dir()),
        "CONTROL_STATE_DIR must be an isolated temporary directory"
    );
    fs::create_dir_all(&state_dir).unwrap();
    let profile = crate::worker_profile::Profile::inspect(&image).unwrap();
    let (mut state, run, _, time) = super::super::tests::fixture();
    let node: NodeId = format!("copy-qa-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()).try_into().unwrap();
    super::super::admit_profile(
        &mut state,
        super::super::Request { run: &run, generation: "attempt-a", nonce: "copy-probe" },
        super::super::Input { command: "sleep 300".to_owned().try_into().unwrap(), lease_seconds: 240, storage_limit_mb: None, shape: None },
        node.clone(),
        time,
        profile,
        |_| Ok(()),
    )
    .unwrap();
    let mut service = state.services.remove(0);
    service.resource.image = image.try_into().unwrap();
    service.resource.phase = Phase::Creating;
    struct Cleanup(Option<ContainerId>, Option<String>, PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(id) = &self.0 {
                let _ = crate::engine::query(&["rm", "-f", id.as_str()]);
            }
            if let Some(volume) = &self.1 {
                let _ = crate::engine::query(&["volume", "rm", volume]);
            }
            let _ = fs::remove_dir_all(&self.2);
        }
    }
    let workspace = crate::runtime::workspace("attempt-a").unwrap();
    let mut cleanup = Cleanup(None, super::super::workspace::Workspace::of(&service).map(|w| w.name().to_owned()), workspace.parent().unwrap().to_owned());
    let id = Docker.create(&service).unwrap();
    cleanup.0 = Some(id.clone());
    service.resource.phase = Phase::Starting(id.clone());
    let observations = Docker.inventory(node.as_str()).unwrap();
    Docker.start(&verify(&service, &observations[id.as_str()]).unwrap()).unwrap();
    service.resource.phase = Phase::Running(id.clone());
    state.services.push(service.clone());

    // An approved project folder and agent-made files in the task workspace.
    fs::create_dir_all(&workspace).unwrap();
    let root = Scratch(workspace.canonicalize().unwrap());
    root.write("mc-client/README.md", b"reusable client\n");
    let run_script = root.write("mc-client/bin/run", b"#!/bin/sh\necho client-ok\n");
    fs::set_permissions(&run_script, fs::Permissions::from_mode(0o755)).unwrap();
    let long = format!("mc-client/{}/{}/assets.bin", "a".repeat(70), "b".repeat(70));
    root.write(&long, &vec![7u8; 3 * 1024 * 1024]);
    root.write("mc-client/.env", b"TOKEN=secret");
    symlink("/etc/passwd", root.0.join("mc-client/passwd-link")).unwrap();
    std::mem::forget(root);

    let shared = Arc::new(Mutex::new(state));
    let mut threads = Vec::new();
    let copy = |nonce: &str, from: &str, to: &str, threads: &mut Vec<std::thread::JoinHandle<()>>| {
        let admitted = request(&shared, &at(&run, "attempt-a", nonce), &node, input(service.resource.id.as_str(), from, to)).unwrap();
        commands::dispatch(&shared, threads).unwrap();
        for thread in threads.drain(..) {
            thread.join().unwrap();
        }
        let handle = serde_json::from_value(json!({"service_id":service.resource.id.as_str(),"execution_id":admitted["execution_id"]})).unwrap();
        commands::status(&shared, &run, "attempt-a", &node, handle).unwrap()
    };
    let check = |script: &str| crate::engine::query(&["exec", id.as_str(), "bash", "-c", script]).unwrap();

    let done = copy("copy-tree", "/workspace/mc-client", "tools/mc", &mut threads);
    println!("{done}");
    assert_eq!(done["status"], "completed", "{done}");
    assert!(done["output"].as_str().unwrap().starts_with("Copied 3 files and 4 directories"), "{done}");
    assert!(done["output"].as_str().unwrap().contains("passwd-link (symlink or special file)"));
    let user = format!("{}:{}", service.uid, service.gid);
    let observed = check(&format!(
        "set -eu; cd /workspace/tools/mc; test \"$(stat -c %u:%g README.md bin/run {long} .)\" = \"$(printf '{user}\\n%.0s' 1 2 3 4)\"; test \"$(./bin/run)\" = client-ok; test \"$(stat -c %s {long})\" = 3145728; test ! -e .env; test ! -e passwd-link; printf tree-ok",
        long = long.strip_prefix("mc-client/").unwrap()
    ));
    assert_eq!(observed.trim(), "tree-ok");

    // A later copy replaces one file and keeps the rest.
    fs::write(workspace.join("mc-client/README.md"), b"v2\n").unwrap();
    let done = copy("copy-file", "mc-client/README.md", "tools/mc/README.md", &mut threads);
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(check("cat /workspace/tools/mc/README.md; test -x /workspace/tools/mc/bin/run && echo kept").trim(), "v2\nkept");

    // An extraction failure is reported without stopping the service.
    let done = copy("copy-conflict", "mc-client/README.md", "tools/mc/bin", &mut threads);
    assert_eq!(done["status"], "failed", "{done}");
    assert!(done["diagnostic"].as_str().unwrap().contains("Extraction in the service failed"), "{done}");
    assert!(matches!(shared.lock().unwrap().services[0].resource.phase, Phase::Running(_)));
    assert_eq!(check("test -x /workspace/tools/mc/bin/run && echo intact").trim(), "intact");
    drop(cleanup);
}
