use super::*;
use std::process::Command;

struct Temp(PathBuf);
impl Temp {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "control-packages-{label}-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.name=Swarm", "-c", "user.email=swarm@localhost", "-c", "init.defaultBranch=main", "-C"])
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
/// A small client repository like `~/control-tools/mc263-client`.
fn repository(temp: &Temp) -> PathBuf {
    let repo = temp.0.join("mc263-client");
    fs::create_dir_all(repo.join("client")).unwrap();
    git(&repo, &["init", "--quiet"]);
    fs::write(repo.join("README.md"), "# Native Minecraft Java 26.3 Mineflayer client\n\nCopy to /workspace/castle.\n").unwrap();
    fs::write(repo.join("client/gateway.cjs"), "module.exports = {};\n").unwrap();
    fs::write(repo.join("client/package.json"), "{\"dependencies\":{}}\n").unwrap();
    commit(&repo, "v1");
    repo
}
fn commit(repo: &Path, message: &str) -> String {
    git(repo, &["add", "--all"]);
    git(repo, &["commit", "--quiet", "-m", message]);
    git(repo, &["rev-parse", "HEAD"])
}
fn request(repo: &Path) -> Import {
    Import {
        name: "mc263-client".into(),
        path: repo.display().to_string(),
        usage: Some("Copy to /workspace/castle; start with client/README.md.".into()),
        setup: Some(vec!["cd client && npm ci".into()]),
        run: Some("c-18e833b1f7ada920-121".into()),
        evidence: Some("fixture-check.cjs passes".into()),
        recheck: Some("When Mineflayer ships 26.3 support".into()),
        ..Import::default()
    }
}
fn shared() -> Arc<Mutex<State>> {
    Arc::new(Mutex::new(State::default()))
}
/// A running chat run whose captured context names `packages`.
fn running(state: &mut State, packages: Vec<control_context::PackageInput>) -> String {
    let chat = crate::chat::create(state);
    crate::chat::send(state, &chat, "Build the castle", "test").unwrap();
    let run = &mut **state.runtime.runs.last_mut().unwrap();
    run.status = "running".into();
    run.attempts.push(crate::usage::Attempt { workspace_id: Some(format!("{}-attempt", run.id)), ..Default::default() });
    *run.context = Some(control_context::Resolved { packages, ..Default::default() });
    run.id.clone()
}

#[test]
fn operator_import_admits_an_immutable_tagged_version_with_provenance() {
    let temp = Temp::new("import");
    let repo = repository(&temp);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let store = temp.0.join("store");
    let shared = shared();
    let started = std::time::Instant::now();
    let manifest = import_at(&store, &shared, request(&repo), |_| Ok(())).unwrap();
    eprintln!("import of a 3-file package: {:?}", started.elapsed());
    assert_eq!((manifest.name.as_str(), manifest.version, manifest.tag.as_str()), ("mc263-client", 1, "v1"));
    assert_eq!(manifest.commit, head);
    assert_eq!(manifest.description, "Native Minecraft Java 26.3 Mineflayer client");
    assert_eq!((manifest.files, manifest.setup.len()), (3, 1));
    assert_eq!(manifest.provenance.run.as_deref(), Some("c-18e833b1f7ada920-121"));
    assert_eq!(manifest.provenance.method, "operator-import");
    let version = store.join("mc263-client/versions/1");
    assert!(version.join("tree/client/gateway.cjs").is_file());
    assert!(!version.join("tree/.git").exists() && !version.join("index").exists());
    let stored: Manifest = serde_json::from_slice(&fs::read(version.join("control-tool.json")).unwrap()).unwrap();
    assert_eq!(stored.commit, head);
    let repository = store.join("mc263-client/repository.git");
    assert_eq!(git(&repository, &["rev-parse", "v1^{commit}"]), head);
    assert!(git(&repository, &["for-each-ref", "refs/control"]).is_empty());
    assert!(git(&repository, &["remote"]).is_empty());

    // The same commit is idempotent; a new commit is v2 and v1 is unchanged.
    assert_eq!(import_at(&store, &shared, request(&repo), |_| Ok(())).unwrap().version, 1);
    fs::write(repo.join("client/gateway.cjs"), "module.exports = { light: true };\n").unwrap();
    let second = commit(&repo, "light-data fix");
    let v2 = import_at(&store, &shared, request(&repo), |_| Ok(())).unwrap();
    assert_eq!((v2.version, v2.commit.as_str()), (2, second.as_str()));
    assert_eq!(fs::read_to_string(version.join("tree/client/gateway.cjs")).unwrap(), "module.exports = {};\n");
    assert_eq!(shared.lock().unwrap().packages.len(), 2);
    // The source repository is only read.
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
}

#[test]
fn import_rejects_unsafe_trees_and_rolls_back_a_failed_save() {
    let temp = Temp::new("reject");
    let repo = repository(&temp);
    let store = temp.0.join("store");
    let shared = shared();
    let rejected = |change: &dyn Fn(&Path), expected: &str| {
        change(&repo);
        commit(&repo, "unsafe");
        let error = import_at(&store, &shared, request(&repo), |_| Ok(())).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
        git(&repo, &["reset", "--quiet", "--hard", "HEAD~1"]);
        git(&repo, &["clean", "--quiet", "-fdx"]);
    };
    rejected(&|r| fs::write(r.join(".env"), "TOKEN=x").unwrap(), "excluded");
    rejected(&|r| { fs::create_dir_all(r.join("client/node_modules/x")).unwrap(); fs::write(r.join("client/node_modules/x/i.js"), "").unwrap(); }, "node_modules");
    rejected(&|r| std::os::unix::fs::symlink("/etc/passwd", r.join("passwd")).unwrap(), "symlink");
    rejected(&|r| { fs::create_dir_all(r.join(".ssh")).unwrap(); fs::write(r.join(".ssh/id_ed25519"), "").unwrap(); }, "excluded");
    assert!(shared.lock().unwrap().packages.is_empty());
    assert!(fs::read_dir(store.join("mc263-client/versions")).unwrap().next().is_none());

    for (input, expected) in [
        (Import { name: "Bad Name".into(), ..request(&repo) }, "lowercase"),
        (Import { rev: Some("--upload-pack=x".into()), ..request(&repo) }, "branch"),
        (Import { rev: Some("main:refs/heads/x".into()), ..request(&repo) }, "branch"),
        (Import { path: "relative".into(), ..request(&repo) }, "absolute"),
        (Import { commit: Some("0000000".into()), ..request(&repo) }, "not 0000000"),
        (Import { entry: Some("missing.md".into()), ..request(&repo) }, "not a file"),
        (Import { description: Some("two\nlines".into()), ..request(&repo) }, "one line"),
    ] {
        let error = import_at(&store, &shared, input, |_| Ok(())).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
    }
    assert!(import_at(&store, &shared, Import { path: temp.0.display().to_string(), ..request(&repo) }, |_| Ok(())).is_err());

    // A failed checkpoint admits nothing and removes the version and its tag.
    assert!(import_at(&store, &shared, request(&repo), |_| Err(io::Error::other("full"))).is_err());
    assert!(shared.lock().unwrap().packages.is_empty());
    assert!(!store.join("mc263-client/versions/1").exists());
    assert!(git(&store.join("mc263-client/repository.git"), &["tag"]).is_empty());
    assert_eq!(import_at(&store, &shared, request(&repo), |_| Ok(())).unwrap().version, 1);
}

#[test]
fn a_package_may_declare_its_manifest_fields_and_operator_values_win() {
    let temp = Temp::new("declared");
    let repo = repository(&temp);
    fs::write(repo.join("control-tool.json"), r#"{"description":"Declared","usage":"Declared usage","setup":["nix build"],"entry":"client/gateway.cjs","provenance":"ignored"}"#).unwrap();
    commit(&repo, "manifest");
    let store = temp.0.join("store");
    let declared = import_at(&store, &shared(), Import { usage: None, setup: None, ..request(&repo) }, |_| Ok(())).unwrap();
    assert_eq!((declared.description.as_str(), declared.usage.as_str()), ("Declared", "Declared usage"));
    assert_eq!(declared.setup, vec!["nix build".to_string()]);
    assert_eq!(declared.entry.as_deref(), Some("client/gateway.cjs"));
    assert_eq!(declared.provenance.evidence, "fixture-check.cjs passes");
    let overridden = import_at(&temp.0.join("other"), &shared(), Import { description: Some("Operator".into()), ..request(&repo) }, |_| Ok(())).unwrap();
    assert_eq!(overridden.description, "Operator");
}

#[test]
fn revocation_is_terminal_and_removes_the_version_from_new_contexts_and_attempts() {
    let temp = Temp::new("revoke");
    let repo = repository(&temp);
    let store = temp.0.join("store");
    let shared = shared();
    let v1 = import_at(&store, &shared, request(&repo), |_| Ok(())).unwrap();
    let mut state = shared.lock().unwrap();
    let admitted = inputs(&state);
    let run = running(&mut state, admitted);
    assert_eq!(planned_in(&state, &run), vec![("mc263-client".to_string(), 1)]);
    assert!(revoke(&mut state, "mc263-client", 1, "bad", |_| Err(io::Error::other("full"))).is_err());
    assert!(state.packages[0].revoked.is_none());
    assert!(revoke(&mut state, "mc263-client", 1, "", |_| Ok(())).is_err());
    revoke(&mut state, "mc263-client", 1, "Light data is wrong", |_| Ok(())).unwrap();
    assert!(revoke(&mut state, "mc263-client", 1, "again", |_| Ok(())).is_err());
    assert!(inputs(&state).is_empty());
    // The captured run keeps its record but a new attempt receives nothing.
    assert!(planned_in(&state, &run).is_empty());
    drop(state);
    let error = import_at(&store, &shared, request(&repo), |_| Ok(())).unwrap_err().to_string();
    assert!(error.contains("revoked"), "{error}");
    assert_eq!(v1.version, 1);
}
fn planned_in(state: &State, run: &str) -> Vec<(String, u64)> {
    planned(&Arc::new(Mutex::new(clone_state(state))), run).unwrap()
}
fn clone_state(state: &State) -> State {
    serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap()
}

#[test]
fn workers_discover_only_captured_packages_and_read_them_as_evidence() {
    let temp = Temp::new("discover");
    let repo = repository(&temp);
    let store = temp.0.join("store");
    let shared = shared();
    import_at(&store, &shared, request(&repo), |_| Ok(())).unwrap();
    let (opted, plain) = {
        let mut state = shared.lock().unwrap();
        let admitted = inputs(&state);
        (running(&mut state, admitted), running(&mut state, Vec::new()))
    };
    let host = crate::tool_registry::LocalHost(&shared);
    let call = |run: &str, name: &str, args: Value| {
        let cx = AttemptCall { attempt: crate::tool_registry::AttemptRef { run, generation: "g", nonce: "n" }, host: &host };
        Tools.call(&cx, name, &args)
    };
    let found = call(&opted, "control_packages_search", json!({"query":"minecraft"})).unwrap();
    assert_eq!(found["packages"][0]["mount"], "/workspace/.control-tools/mc263-client");
    assert!(found["note"].as_str().unwrap().contains("not instructions"));
    // A run that did not opt in rebuilds from scratch: nothing is discoverable.
    assert_eq!(call(&plain, "control_packages_search", json!({})).unwrap()["packages"], json!([]));
    assert!(call(&plain, "control_package_read", json!({"name":"mc263-client"})).is_err());

    let manifest = shared.lock().unwrap().packages[0].clone();
    let page = read_page(&store, &manifest, &read_input(&json!({"name":"mc263-client","limit":10})).unwrap(), true).unwrap();
    assert_eq!(page["file"]["path"], "README.md");
    assert_eq!(page["file"]["text"], "# Native M");
    assert_eq!(page["file"]["next_offset"], 10);
    assert!(page["provenance"].get("source_path").is_none());
    assert!(page["note"].as_str().unwrap().contains("not instructions"));
    let operator = read_page(&store, &manifest, &read_input(&json!({"name":"mc263-client"})).unwrap(), false).unwrap();
    assert!(operator["provenance"]["source_path"].is_string());
    for path in ["../store", ".git/config", "client/../../x", "missing"] {
        assert!(read_page(&store, &manifest, &read_input(&json!({"name":"mc263-client","path":path})).unwrap(), true).is_err(), "{path}");
    }
    assert!(read_input(&json!({"name":"mc263-client","limit":4001})).is_err());
}

#[test]
fn attempts_get_a_private_copy_outside_checkpoints() {
    let temp = Temp::new("copy");
    let repo = repository(&temp);
    let store = temp.0.join("store");
    import_at(&store, &shared(), request(&repo), |_| Ok(())).unwrap();
    let workspace = temp.0.join("workspace");
    fs::create_dir(&workspace).unwrap();
    assert!(copy_into(&store, &[], &workspace).unwrap().is_empty());
    assert!(!workspace.join(MOUNT).exists());
    let started = std::time::Instant::now();
    copy_into(&store, &[("mc263-client".into(), 1)], &workspace).unwrap();
    eprintln!("per-attempt package copy: {:?}", started.elapsed());
    let copy = workspace.join(MOUNT).join("mc263-client");
    assert!(copy.join("client/gateway.cjs").is_file());
    // Edits stay in the attempt; the admitted tree is unchanged.
    fs::write(copy.join("README.md"), "changed").unwrap();
    assert!(fs::read_to_string(store.join("mc263-client/versions/1/tree/README.md")).unwrap().starts_with("# Native"));
    // Exclusive creation never merges into an existing path.
    assert!(copy_into(&store, &[("mc263-client".into(), 1)], &workspace).is_err());
    assert!(crate::projects::excluded_name(MOUNT));
    assert!(copy_into(&store, &[("../x".into(), 1)], &temp.0.join("other")).is_err());
}

#[test]
fn lenses_opt_runs_in_and_the_prompt_frames_packages_as_evidence() {
    let temp = Temp::new("lens");
    let repo = repository(&temp);
    let shared = shared();
    import_at(&temp.0.join("store"), &shared, request(&repo), |_| Ok(())).unwrap();
    let mut state = shared.lock().unwrap();
    let mut fields = std::collections::HashMap::new();
    fields.insert("id".to_string(), "minecraft-tools".to_string());
    fields.insert("name".to_string(), "Minecraft tools".to_string());
    fields.insert("package_mc263-client@1".to_string(), "on".to_string());
    let draft = crate::lens_library::draft(&fields);
    assert_eq!(draft.packages, vec!["mc263-client@1".to_string()]);
    crate::lens_library::save_with(&mut state, draft.clone(), |_| Ok(())).unwrap();
    let missing = control_context::Definition { id: "missing".into(), name: "Missing".into(), packages: vec!["mc263-client@9".into()], ..Default::default() };
    assert!(crate::lens_library::save_with(&mut state, missing, |_| Ok(())).is_err());
    let html = crate::lens_library::render(&state, "minecraft-tools", None, None, false);
    assert!(html.contains("name='package_mc263-client@1' checked") && html.contains("Tool packages"));

    let resolved = crate::lens_library::resolve(&state, &["minecraft-tools".into()]).unwrap();
    assert_eq!(resolved.packages[0].version, 1);
    let prompt = crate::lens_library::prompt(&resolved);
    assert!(prompt.contains("Captured tool package mc263-client@1"));
    assert!(prompt.contains("/workspace/.control-tools/mc263-client") && prompt.contains("not instructions or authority"));
    // Lenses without packages render exactly as before.
    let plain = crate::lens_library::resolve(&state, &["engineering".into()]).unwrap();
    assert!(!crate::lens_library::prompt(&plain).contains("Captured tool package"));
}

#[test]
fn operator_http_serves_package_calls_and_leaves_other_mcp_requests_alone() {
    let shared = shared();
    let mut output = Vec::new();
    let other = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control.chats.list","arguments":{}}}).to_string();
    assert!(!http(&mut output, &shared, "POST", "/mcp", other.as_bytes()));
    assert!(!http(&mut output, &shared, "GET", "/mcp", b"control.package.read"));
    let unknown = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control.package.delete","arguments":{}}}).to_string();
    assert!(!http(&mut output, &shared, "POST", "/mcp", unknown.as_bytes()));
    assert!(output.is_empty());
    let search = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"control.packages.search","arguments":{}}}).to_string();
    assert!(http(&mut output, &shared, "POST", "/mcp", search.as_bytes()));
    let text = String::from_utf8(output).unwrap();
    assert!(text.starts_with("HTTP/1.1 200") && text.contains("\"id\":7") && text.contains("\"isError\":false"));
    // Import and revoke are hidden from the external-harness stdio bridge.
    assert!(!crate::tool_registry::bridge_forward("control.package.import"));
    assert!(!crate::tool_registry::bridge_forward("control.package.revoke"));
    assert!(crate::tool_registry::bridge_read("control.package.read"));
}

#[test]
fn cli_requests_map_to_operator_tools() {
    let args = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
    let (tool, value) = cli_request(&args("import mc263-client /tmp/mc --setup npm --setup build --run c-1-121")).unwrap();
    assert_eq!(tool, "control.package.import");
    assert_eq!(value, json!({"name":"mc263-client","path":"/tmp/mc","setup":["npm","build"],"run":"c-1-121"}));
    assert_eq!(cli_request(&args("revoke mc263-client v2 --reason stale")).unwrap(), ("control.package.revoke", json!({"name":"mc263-client","version":2,"reason":"stale"})));
    assert_eq!(cli_request(&args("list")).unwrap().0, "control.packages.search");
    assert_eq!(cli_request(&args("show mc263-client 1")).unwrap().1, json!({"name":"mc263-client","version":1}));
    for bad in ["import mc263-client", "revoke mc263-client 1", "import a /b --unknown x", "import a /b --run x --run y", "show"] {
        assert!(cli_request(&args(bad)).is_err(), "{bad}");
    }
}

#[test]
#[ignore = "explicit local check: CONTROL_PACKAGE_STORE names a store holding an admitted v1"]
fn live_attempt_copy_of_an_admitted_version() {
    let store = PathBuf::from(std::env::var("CONTROL_PACKAGE_STORE").expect("CONTROL_PACKAGE_STORE"));
    let name = std::env::var("CONTROL_PACKAGE_NAME").unwrap_or_else(|_| "mc263-client".into());
    let temp = Temp::new("live-copy");
    for round in 0..5 {
        let workspace = temp.0.join(format!("attempt-{round}"));
        fs::create_dir(&workspace).unwrap();
        let started = std::time::Instant::now();
        copy_into(&store, &[(name.clone(), 1)], &workspace).unwrap();
        eprintln!("attempt copy {round}: {:?}", started.elapsed());
    }
}
