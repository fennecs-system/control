
use super::*;
use crate::runtime::RunKind;
use crate::{
    backend::Backend,
    delegation::{Delegation, Input as Task, Persona, Role},
    identity::{Action, Actor},
    usage::Attempt,
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
struct Store(PathBuf);
impl Store {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "control-child-read-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn publish(&self, id: &str, files: &[(&str, &[u8])]) {
        let source = self.0.join(format!("source-{id}"));
        fs::create_dir(&source).unwrap();
        for (name, bytes) in files {
            let path = source.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        artifacts::publish_in(&self.0.join("artifacts"), &source, id).unwrap();
    }
    fn read(&self, state: &State, args: Value) -> Result<Value, &'static str> {
        let plan = prepare(state, "resumed", args)?;
        execute_with(
            plan,
            |id| artifacts::manifest_in(&self.0.join("artifacts"), id),
            |manifest, path| artifacts::read_in(&self.0.join("artifacts"), manifest, path),
        )
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub(crate) fn fixture() -> State {
    let mut s = State::default();
    let parent_chat = crate::chat::create(&mut s);
    let child_chat = crate::chat::create(&mut s);
    s.runtime
        .enqueue_for(
            "Original task".into(),
            RunKind::SelfImprovement,
            "test",
            Actor::LocalOperator,
            Action::Improve,
        )
        .unwrap();
    let parent = &mut s.runtime.runs[0];
    parent.id = "parent".into();
    parent.status = "continued".into();
    parent.chat_id = Some(parent_chat);
    parent.attempts.push(Attempt {
        workspace_id: Some("parent-attempt-1".into()),
        artifact: Some("parent-attempt-1".into()),
        status: "waiting".into(),
        ..Attempt::default()
    });
    parent.delegation = Some(Delegation {
        input: Task {
            model:None,
            objective: "Implement child task".into(),
            role: Role::Worker,
        },
        nonce: "n".into(),
        generation: "parent-attempt-1".into(),
        backend: Backend::Codex,
        model: "dev".into(),
        child: Some("child".into()),
        continuation: Some("resumed".into()),
    });
    let mut child = parent.clone();
    child.id = "child".into();
    child.kind = RunKind::Delegated;
    child.status = "completed".into();
    child.delegation = None;
    child.chat_id = Some(child_chat);
    child.persona = Some(Persona {
        role: Role::Worker,
        root: "parent".into(),
        depth: 1,
        parent: Some("parent".into()),
    });
    child.attempts = vec![Attempt {
        workspace_id: Some("child-attempt-1".into()),
        artifact: Some("child-attempt-1".into()),
        parent_artifact: Some("parent-attempt-1".into()),
        status: "completed".into(),
        ..Attempt::default()
    }];
    child.result = "All requested changes implemented and verified".into();
    let mut resumed = parent.clone();
    resumed.id = "resumed".into();
    resumed.status = "running".into();
    resumed.delegation = None;
    resumed.attempts.clear();
    resumed.persona = Some(Persona {
        role: Role::Worker,
        root: "parent".into(),
        depth: 0,
        parent: None,
    });
    s.runtime.runs.push(child);
    s.runtime.runs.push(resumed);
    s
}
#[test]
fn full_unicode_file_and_report_are_reconstructible_beyond_feedback_excerpt() {
    let store = Store::new();
    let mut s = fixture();
    let text = "rust 🦀 café\n".repeat(12000);
    *s.runtime.runs[1].result = "verified 🦀\n".repeat(3000);
    store.publish("child-attempt-1", &[("src/main.rs", text.as_bytes())]);
    for (section, key, expected) in [
        ("file", "file", text),
        ("summary", "summary", s.runtime.runs[1].result.to_string()),
    ] {
        let mut offset = 0;
        let mut collected = String::new();
        let mut pages = 0;
        loop {
            let mut args = json!({"child":"child","section":section,"offset":offset,"limit":3071});
            if section == "file" {
                args["path"] = json!("src/main.rs");
            }
            let response = store.read(&s, args).unwrap();
            let page = &response[key];
            assert!(page["text"].as_str().unwrap().chars().count() <= 3071);
            assert_eq!(page["offset"], offset);
            collected.push_str(page["text"].as_str().unwrap());
            pages += 1;
            let Some(next) = page["next_offset"].as_u64() else {
                break;
            };
            offset = next;
        }
        assert!(pages > 3);
        assert_eq!(collected, expected);
    }
}
#[test]
fn file_pages_are_stable_sorted_and_include_modified_added_and_deleted_entries() {
    let store = Store::new();
    let s = fixture();
    store.publish(
        "parent-attempt-1",
        &[
            ("changed.rs", b"before"),
            ("deleted.rs", b"gone"),
            ("same.rs", b"same"),
        ],
    );
    let mut owned = (0..130)
        .map(|i| (format!("new/{i:03}.rs"), format!("fn f{i}() {{}}")))
        .collect::<Vec<_>>();
    owned.push(("changed.rs".into(), "after".into()));
    owned.push(("same.rs".into(), "same".into()));
    let refs = owned
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_bytes()))
        .collect::<Vec<_>>();
    store.publish("child-attempt-1", &refs);
    let mut offset = 0;
    let mut entries = Vec::new();
    loop {
        let response = store
            .read(
                &s,
                json!({"child":"child","section":"files","offset":offset,"limit":16384}),
            )
            .unwrap();
        assert_eq!(response["changes_available"], true);
        let page = response["files"].as_array().unwrap();
        assert!(page.len() <= FILES_LIMIT);
        entries.extend(page.clone());
        let Some(next) = response["next_offset"].as_u64() else {
            break;
        };
        offset = next;
    }
    assert_eq!(entries.len(), 133);
    assert!(
        entries
            .windows(2)
            .all(|p| p[0]["path"].as_str() < p[1]["path"].as_str())
    );
    for (path, change) in [
        ("changed.rs", "modified"),
        ("deleted.rs", "deleted"),
        ("same.rs", "unchanged"),
        ("new/000.rs", "added"),
    ] {
        assert_eq!(
            entries.iter().find(|e| e["path"] == path).unwrap()["change"],
            change
        );
    }
    assert!(
        store
            .read(&s, json!({"child":"child","section":"files","offset":134}))
            .is_err()
    );
}
#[test]
fn actor_root_actual_delegation_and_artifact_identity_are_required() {
    let s = fixture();
    for mutation in 0..5 {
        let mut s: State = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        match mutation {
            0 => s.runtime.runs[1].actor = Actor::Unknown,
            1 => s.runtime.runs[1].persona.as_mut().unwrap().root = "other-root".into(),
            2 => s.runtime.runs[1].persona.as_mut().unwrap().parent = Some("forged-parent".into()),
            3 => s.runtime.runs[0].delegation.as_mut().unwrap().child = None,
            _ => s.chats[1].actor = Actor::Unknown,
        }
        assert!(prepare(&s, "resumed", json!({"child":"child","section":"files"})).is_err());
    }
    let mut s = fixture();
    s.runtime.runs[1].attempts[0].artifact = Some("other-chat-artifact".into());
    assert!(prepare(&s, "resumed", json!({"child":"child","section":"files"})).is_err());
    assert!(
        prepare(
            &fixture(),
            "resumed",
            json!({"child":"parent","section":"summary"})
        )
        .is_err()
    );
    s.runtime.runs[1].attempts[0].workspace_id = Some("other-chat-artifact".into());
    assert!(prepare(&s, "resumed", json!({"child":"child","section":"files"})).is_err());
    let mut s = fixture();
    s.runtime.runs[0].delegation.as_mut().unwrap().generation = "forged-source".into();
    assert!(prepare(&s, "resumed", json!({"child":"child","section":"summary"})).is_err());
    assert!(
        prepare(
            &fixture(),
            "resumed",
            json!({"child":"resumed","section":"summary"})
        )
        .is_err()
    );
}
#[test]
fn sibling_and_unrelated_same_actor_chats_do_not_gain_access() {
    let mut s = fixture();
    let mut sibling = s.runtime.runs[1].clone();
    sibling.id = "sibling".into();
    sibling.status = "running".into();
    sibling.persona.as_mut().unwrap().role = Role::Judge;
    let sibling_chat = crate::chat::create(&mut s);
    sibling.chat_id = Some(sibling_chat);
    s.runtime.runs.push(sibling);
    assert!(prepare(&s, "sibling", json!({"child":"child","section":"files"})).is_err());
    assert!(
        prepare(
            &s,
            "resumed",
            json!({"child":"sibling","section":"summary"})
        )
        .is_err()
    );
    let mut unrelated = s.runtime.runs[2].clone();
    unrelated.id = "unrelated".into();
    unrelated.persona = None;
    s.runtime.runs.push(unrelated);
    assert!(prepare(&s, "unrelated", json!({"child":"child","section":"files"})).is_err());
}
#[test]
fn live_failed_and_missing_artifacts_never_fall_back_to_workspaces() {
    for status in ["running", "waiting", "failed", "interrupted", "cancelled"] {
        let mut s = fixture();
        s.runtime.runs[1].status = status.into();
        assert!(prepare(&s, "resumed", json!({"child":"child","section":"summary"})).is_err());
    }
    let mut s = fixture();
    s.runtime.runs[1].attempts[0].artifact = None;
    assert!(prepare(&s, "resumed", json!({"child":"child","section":"files"})).is_err());
    let plan = prepare(&s, "resumed", json!({"child":"child","section":"summary"})).unwrap();
    assert!(
        execute_with(
            plan,
            |_| panic!("Report must not read files"),
            |_, _| panic!("Report must not read files")
        )
        .is_ok()
    );
}
#[test]
fn paths_limits_hash_changes_and_binary_files_fail_closed() {
    let store = Store::new();
    let s = fixture();
    store.publish(
        "child-attempt-1",
        &[("safe.rs", b"original"), ("binary", b"\0binary")],
    );
    fs::write(
        store.0.join("source-child-attempt-1/safe.rs"),
        b"live workspace changed",
    )
    .unwrap();
    assert_eq!(
        store
            .read(
                &s,
                json!({"child":"child","section":"file","path":"safe.rs"})
            )
            .unwrap()["file"]["text"],
        "original"
    );
    for path in [
        "../safe.rs",
        "/etc/passwd",
        ".env",
        ".codex/auth.json",
        "missing.rs",
        "binary",
    ] {
        assert!(
            store
                .read(&s, json!({"child":"child","section":"file","path":path}))
                .is_err()
        );
    }
    for args in [
        json!({"child":"child","section":"summary","path":"safe.rs"}),
        json!({"child":"child","section":"file"}),
        json!({"child":"child","limit":16385}),
        json!({"child":"child","limit":0}),
        json!({"child":"child","artifact":"forged"}),
        json!({"child":"child","offset":-1}),
    ] {
        assert!(prepare(&s, "resumed", args).is_err());
    }
    assert!(
        store
            .read(
                &s,
                json!({"child":"child","section":"file","path":"safe.rs","offset":9})
            )
            .is_err()
    );
    fs::write(
        store.0.join("artifacts/child-attempt-1/files/safe.rs"),
        b"modified",
    )
    .unwrap();
    assert!(
        store
            .read(
                &s,
                json!({"child":"child","section":"file","path":"safe.rs"})
            )
            .is_err()
    );
}
#[test]
fn child_continuations_are_followed_but_forged_cross_chat_chains_are_denied() {
    let store = Store::new();
    let mut s = fixture();
    store.publish(
        "child-continuation-attempt-1",
        &[("final.rs", b"final child text")],
    );
    let mut next = s.runtime.runs[1].clone();
    next.id = "child-continuation".into();
    next.attempts[0].workspace_id = Some("child-continuation-attempt-1".into());
    next.attempts[0].artifact = Some("child-continuation-attempt-1".into());
    s.runtime.runs[1].status = "continued".into();
    let mut d = s.runtime.runs[0].delegation.clone().unwrap();
    d.child = None;
    d.continuation = Some(next.id.clone());
    s.runtime.runs[1].delegation = Some(d);
    s.runtime.runs.push(next);
    let value = store
        .read(
            &s,
            json!({"child":"child","section":"file","path":"final.rs"}),
        )
        .unwrap();
    assert_eq!(value["run"], "child-continuation");
    assert_eq!(value["file"]["text"], "final child text");
    s.runtime.runs[3].chat_id = s.runtime.runs[0].chat_id.clone();
    assert!(prepare(&s, "resumed", json!({"child":"child","section":"summary"})).is_err());
}
