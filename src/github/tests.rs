
use super::*;
use std::cell::RefCell;
const BASE: &str = "1111111111111111111111111111111111111111";
const TREE: &str = "2222222222222222222222222222222222222222";
const NEW_TREE: &str = "3333333333333333333333333333333333333333";
const COMMIT: &str = "4444444444444444444444444444444444444444";
struct Mock {
    requests: RefCell<Vec<(String, Option<Value>)>>,
    reference: RefCell<Option<String>>,
    pull: RefCell<Option<Value>>,
    fail: RefCell<Option<&'static str>>,
    old: Vec<u8>,
    base: String,
    tree_entries: Vec<Value>,
}
impl Mock {
    fn new() -> Self {
        Self {
            requests: RefCell::new(Vec::new()),
            reference: RefCell::new(None),
            pull: RefCell::new(None),
            fail: RefCell::new(None),
            old: b"old\n".to_vec(),
            base: BASE.into(),
            tree_entries: vec![
                json!({"path":"script.sh","sha":git_blob(b"old\n"),"mode":"100755"}),
            ],
        }
    }
}
impl Api for Mock {
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> io::Result<Value> {
        assert!(path.starts_with("/repos/example/project/"));
        assert!(
            !["PATCH", "DELETE", "PUT"].contains(&method),
            "No overwrite, merge or deletion endpoints"
        );
        self.requests
            .borrow_mut()
            .push((format!("{method} {path}"), body.clone()));
        let result = if method == "GET" && path.contains("git/commits/") {
            json!({"tree":{"sha":TREE}})
        } else if method == "GET" && path.contains("contents/") {
            json!({"type":"file","sha":git_blob(&self.old)})
        } else if method == "GET" && path.contains("git/trees/") {
            json!({"tree":self.tree_entries,"truncated":false})
        } else if method == "POST" && path.ends_with("git/trees") {
            json!({"sha":NEW_TREE})
        } else if method == "POST" && path.ends_with("git/commits") {
            json!({"sha":COMMIT})
        } else if method == "GET" && path.ends_with("git/ref/heads/main") {
            json!({"object":{"sha":self.base}})
        } else if method == "GET" && path.contains("git/ref/") {
            self.reference
                .borrow()
                .as_ref()
                .map_or(Value::Null, |sha| json!({"object":{"sha":sha}}))
        } else if method == "POST" && path.ends_with("git/refs") {
            *self.reference.borrow_mut() =
                body.as_ref().unwrap()["sha"].as_str().map(str::to_owned);
            json!({"object":{"sha":COMMIT}})
        } else if method == "GET" && path.contains("pulls?") {
            self.pull
                .borrow()
                .as_ref()
                .map_or(json!([]), |p| json!([p]))
        } else if method == "POST" && path.ends_with("pulls") {
            let value = json!({"html_url":"https://github.com/example/project/pull/7"});
            *self.pull.borrow_mut() = Some(value.clone());
            value
        } else {
            panic!("Unexpected mock route: {method} {path}")
        };
        if self
            .fail
            .borrow()
            .is_some_and(|suffix| method == "POST" && path.ends_with(suffix))
        {
            self.fail.borrow_mut().take();
            return Err(fail("Synthetic lost acknowledgement"));
        }
        Ok(result)
    }
}
fn publication() -> Publication {
    Publication {
        run: "r-fixture".into(),
        artifact: "a-fixture".into(),
        repository: "example/project".into(),
        branch: "control/r-fixture".into(),
        base: "main".into(),
        base_commit: BASE.into(),
        tree: None,
        commit: None,
        url: None,
        status: "preparing".into(),
        title: "Reviewable change".into(),
        body: "Synthetic verification".into(),
    }
}
fn edit() -> Vec<Change> {
    vec![Change {
        path: "script.sh".into(),
        old: Some(b"old\n".to_vec()),
        new: Some(b"new\n".to_vec()),
    }]
}
#[test]
fn tree_cache_reuses_listings_and_preserves_each_files_mode() {
    let mut api = Mock::new();
    api.tree_entries
        .push(json!({"path":"module.rs","sha":git_blob(&api.old),"mode":"100644"}));
    let mut changes = edit();
    changes.push(Change {
        path: "module.rs".into(),
        old: Some(api.old.clone()),
        new: Some(b"updated".to_vec()),
    });
    create_tree(&api, &publication(), &changes).unwrap();
    let requests = api.requests.borrow();
    assert_eq!(
        requests
            .iter()
            .filter(|(path, _)| path.contains("GET /repos/example/project/git/trees/"))
            .count(),
        1
    );
    let body = requests
        .iter()
        .find(|(path, _)| path == "POST /repos/example/project/git/trees")
        .unwrap()
        .1
        .as_ref()
        .unwrap();
    assert_eq!(body["tree"][0]["mode"], "100755");
    assert_eq!(body["tree"][1]["mode"], "100644");
}
#[test]
fn github_objects_and_refs_are_scoped_and_unambiguous() {
    for value in [
        "x/y/z",
        "../repo",
        "x/..",
        "https://github.com/x/y",
        "x/y?token=bad",
    ] {
        assert!(!repository(value));
    }
    for value in [
        "main..other",
        "/main",
        "a/.hidden",
        "main.lock",
        "refs@{1}",
        "main?x=y",
    ] {
        assert!(!branch(value));
    }
    assert!(repository("some-owner/a_repo"));
    assert!(branch("control/r-fixture"));
    assert_eq!(
        git_blob(b"test content\n"),
        "d670460b4b4aece5915caf5c68d12f560a9fe3e4"
    );
    assert_eq!(encoded("control/r?x"), "control%2Fr%3Fx");
    assert!(
        serde_json::from_value::<Input>(
            json!({"run_id":"r-x","title":"x","repository":"other/repo"})
        )
        .is_err()
    );
}
#[test]
fn publication_preserves_mode_and_creates_only_a_draft_without_overwriting() {
    let api = Mock::new();
    let mut publication = publication();
    let mut saved = Vec::new();
    let url = finish(&api, &mut publication, &edit(), |p| {
        saved.push(p.clone());
        Ok(())
    })
    .unwrap();
    assert_eq!(url, "https://github.com/example/project/pull/7");
    let requests = api.requests.borrow();
    let tree = requests
        .iter()
        .find(|(p, _)| p.ends_with("git/trees") && p.starts_with("POST"))
        .unwrap()
        .1
        .as_ref()
        .unwrap();
    assert_eq!(tree["base_tree"], TREE);
    assert_eq!(tree["tree"][0]["mode"], "100755");
    assert_eq!(tree["tree"][0]["content"], "new\n");
    let pull = requests
        .iter()
        .find(|(p, _)| p.ends_with("pulls") && p.starts_with("POST"))
        .unwrap()
        .1
        .as_ref()
        .unwrap();
    assert_eq!(pull["draft"], true);
    assert_eq!(pull["base"], "main");
    assert!(
        saved
            .iter()
            .any(|p| p.commit.as_deref() == Some(COMMIT) && p.status == "preparing")
    );
}
#[test]
fn lost_branch_or_pr_acknowledgement_is_reconciled_without_duplicate_pr() {
    for suffix in ["git/refs", "pulls"] {
        let api = Mock::new();
        *api.fail.borrow_mut() = Some(suffix);
        let mut publication = publication();
        let mut durable = publication.clone();
        assert!(
            finish(&api, &mut publication, &edit(), |p| {
                durable = p.clone();
                Ok(())
            })
            .is_err()
        );
        finish(&api, &mut durable, &edit(), |_| Ok(())).unwrap();
        assert_eq!(
            api.requests
                .borrow()
                .iter()
                .filter(|(p, _)| p == "POST /repos/example/project/pulls")
                .count(),
            1
        );
    }
}
#[test]
fn changed_source_or_replaced_branch_is_never_overwritten() {
    let mut api = Mock::new();
    api.old = b"different\n".to_vec();
    let mut publication = publication();
    assert!(
        finish(&api, &mut publication, &edit(), |_| Ok(()))
            .unwrap_err()
            .to_string()
            .contains("baseline differs")
    );
    assert!(
        api.requests
            .borrow()
            .iter()
            .all(|(p, _)| p.starts_with("GET"))
    );
    let api = Mock::new();
    *api.reference.borrow_mut() = Some("5555555555555555555555555555555555555555".into());
    let mut publication = super::tests::publication();
    publication.tree = Some(NEW_TREE.into());
    publication.commit = Some(COMMIT.into());
    assert!(
        finish(&api, &mut publication, &edit(), |_| Ok(()))
            .unwrap_err()
            .to_string()
            .contains("not overwritten")
    );
    assert!(api.pull.borrow().is_none());
}
#[test]
fn diverged_remote_base_prevents_every_external_write() {
    let mut api = Mock::new();
    api.base = "5555555555555555555555555555555555555555".into();
    let mut publication = publication();
    assert!(
        finish(&api, &mut publication, &edit(), |_| Ok(()))
            .unwrap_err()
            .to_string()
            .contains("base differs")
    );
    assert!(
        api.requests
            .borrow()
            .iter()
            .all(|(method, _)| method.starts_with("GET"))
    );
    assert!(publication.tree.is_none());
    assert!(api.reference.borrow().is_none());
    assert!(api.pull.borrow().is_none());
}
#[test]
fn cumulative_turn_keeps_original_source_without_recapturing_host() {
    let source = Source {
        repository: "example/project".into(),
        source_root: "/operator-source".into(),
        commit: BASE.into(),
        baseline: "original-verified-baseline".into(),
    };
    let mut attempt = Attempt {
        github_source: Some(source.clone()),
        ..Default::default()
    };
    prepare(
        &mut attempt,
        Path::new("/deliberately-unavailable-later-baseline"),
    )
    .unwrap();
    assert_eq!(attempt.github_source, Some(source));
}
#[test]
fn source_capture_rejects_dirty_untracked_ignored_and_stale_snapshot_inputs() {
    let root = std::env::temp_dir().join(format!(
        "control-github-source-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    source_git(&root, &["init", "--quiet"]).unwrap();
    fs::write(root.join("file.txt"), b"committed\n").unwrap();
    fs::write(root.join(".gitignore"), b"ignored.txt\n").unwrap();
    source_git(&root, &["add", "--", "file.txt", ".gitignore"]).unwrap();
    source_git(
        &root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "Fixture",
        ],
    )
    .unwrap();
    let read = |path: &str| fs::read(root.join(path));
    assert!(verify_source(&root, &["file.txt"], read).is_ok());
    fs::write(root.join("file.txt"), b"dirty\n").unwrap();
    assert!(
        verify_source(&root, &["file.txt"], read)
            .unwrap_err()
            .to_string()
            .contains("clean committed")
    );
    fs::write(root.join("file.txt"), b"committed\n").unwrap();
    fs::write(root.join("untracked.txt"), b"untracked\n").unwrap();
    assert!(
        verify_source(&root, &["file.txt"], read)
            .unwrap_err()
            .to_string()
            .contains("clean committed")
    );
    fs::remove_file(root.join("untracked.txt")).unwrap();
    fs::write(root.join("ignored.txt"), b"ignored dependency\n").unwrap();
    assert!(
        source_git(
            &root,
            &["status", "--porcelain=v1", "--untracked-files=normal"]
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        verify_source(&root, &["file.txt", "ignored.txt"], read)
            .unwrap_err()
            .to_string()
            .contains("ignored input")
    );
    assert!(
        verify_source(&root, &["file.txt"], |_| Ok(b"stale snapshot\n".to_vec()))
            .unwrap_err()
            .to_string()
            .contains("committed Git objects")
    );
}
#[test]
fn disk_failure_prevents_a_reachable_branch_or_pull_request() {
    let api = Mock::new();
    let mut publication = publication();
    assert!(
        finish(&api, &mut publication, &edit(), |_| Err(fail(
            "Synthetic disk full"
        )))
        .is_err()
    );
    assert!(api.reference.borrow().is_none());
    assert!(api.pull.borrow().is_none());
}
#[test]
fn only_completed_same_chat_improvements_can_be_published() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "/improve Implement a fixture", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "completed".into();
    run.attempts.push(Attempt {
        status: "completed".into(),
        artifact: Some("a-fixture".into()),
        github_source: Some(Source {
            repository: "example/project".into(),
            source_root: "/fixture".into(),
            commit: BASE.into(),
            baseline: "original".into(),
        }),
        ..Default::default()
    });
    let input = Input {
        run_id: run.id.clone(),
        title: "Fixture".into(),
        body: String::new(),
    };
    assert!(selected(&state, &chat, &input).is_ok());
    state.runtime.runs[0].attempts[0].review_only = true;
    assert!(selected(&state, &chat, &input).is_err());
    state.runtime.runs[0].attempts[0].review_only = false;
    state.runtime.runs[0].persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Judge,
        root: "fixture".into(),
        depth: 1,
        parent: Some("parent".into()),
    });
    assert!(selected(&state, &chat, &input).is_err());
    state.runtime.runs[0].persona = None;
    let other = crate::chat::create(&mut state);
    assert!(selected(&state, &other, &input).is_err());
    state.runtime.runs[0].action = Action::Task;
    assert!(selected(&state, &chat, &input).is_err());
    state.runtime.runs[0].action = Action::Improve;
    state.runtime.runs[0].status = "running".into();
    assert!(selected(&state, &chat, &input).is_err());
    state.chats[0].actor = Actor::Unknown;
    assert!(selected(&state, &chat, &input).is_err());
}
