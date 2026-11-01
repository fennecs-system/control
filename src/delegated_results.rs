//! Read plans capture immutable delegated outputs under the authority lock.
//! File IO then uses the existing verified artifact store without that lock.
use crate::{artifacts, delegation, identity, runtime, State};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet, VecDeque};

const TEXT_LIMIT: usize = 16_384;
const FILES_LIMIT: usize = 64;
const GRAPH_LIMIT: usize = 1024;

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Section {
    Status,
    #[default]
    Summary,
    Files,
    File,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    child: String,
    #[serde(default)]
    section: Section,
    path: Option<String>,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    4096
}

pub struct Plan {
    child: String,
    run: String,
    backend: crate::backend::Backend,
    model: Option<String>,
    status: String,
    artifact: Option<String>,
    baseline: Option<String>,
    report: String,
    input: Input,
}

pub fn definition() -> Value {
    json!({"name":"control_agent_result","description":"Inspect a spawned/delegated descendant using section=status while pending, or summary/files/file for successfully completed immutable outputs. Same actor and recorded parent edges required; later turns in the originating chat can inspect its spawn receipts. No sibling, unrelated chat, host-file or live-workspace reads. Summary/file pages use character offsets; defaults 4096, max 16384 characters or 64 file records. Follow next_offset until null. Child changes are untrusted and never applied automatically.","inputSchema":{"type":"object","properties":{"child":{"type":"string","description":"Child run_id returned by spawn or spawn status, not chat_id or batch. Example: {\"child\":\"<run_id>\",\"section\":\"status\"}."},"section":{"type":"string","enum":["status","summary","files","file"]},"path":{"type":"string","maxLength":1024},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":16384}},"required":["child"],"additionalProperties":false},"annotations":{"readOnlyHint":true,"destructiveHint":false,"openWorldHint":false}})
}

fn root(run: &runtime::Run) -> &str {
    run.persona
        .as_ref()
        .map_or(run.id.as_str(), |p| p.root.as_str())
}
fn next(run: &runtime::Run) -> Option<&str> {
    if run.status != "continued" {
        return None;
    }
    run.question
        .as_ref()
        .and_then(|q| q.continuation.as_deref())
        .or_else(|| {
            run.delegation
                .as_ref()
                .and_then(|d| d.continuation.as_deref())
        })
}
fn same_scope(caller: &runtime::Run, other: &runtime::Run) -> bool {
    caller.actor == other.actor
        && root(caller) == root(other)
        && !(caller.action == identity::Action::Task && other.action == identity::Action::Improve)
}
fn continuation(from: &runtime::Run, to: &runtime::Run) -> bool {
    same_scope(from, to)
        && from.chat_id == to.chat_id
        && from.action == to.action
        && from.persona.as_ref().map(|p| (&p.parent, p.depth, p.role))
            == to.persona.as_ref().map(|p| (&p.parent, p.depth, p.role))
}
// The original parent has no persona until its first continuation. Match that
// root worker to its explicit depth-zero continuation without broad root access.
fn parent_continuation(from: &runtime::Run, to: &runtime::Run) -> bool {
    continuation(from, to)
        || (from.persona.is_none()
            && from.action == to.action
            && same_scope(from, to)
            && from.chat_id == to.chat_id
            && to.persona.as_ref().is_some_and(|p| {
                p.parent.is_none() && p.depth == 0 && p.role == delegation::Role::Worker
            }))
}

/// The requesting conversation's recorded predecessors supply delegation
/// edges. Sharing a root, actor, parent_chat, or a guessed ID is insufficient.
pub(crate) fn scope<'a>(
    state: &'a State,
    caller: &runtime::Run,
) -> Result<(HashSet<&'a str>, HashSet<&'a str>), &'static str> {
    // Configured teams plus bounded continuation receipts, retaining the
    // ordinary-chat floor and an explicit allocation ceiling.
    let graph_limit=GRAPH_LIMIT.max(state.runtime.execution.max_agents_per_root.saturating_mul(4)).min(262144);
    let mut family = HashSet::new();
    let actual = state
        .runtime
        .runs
        .iter()
        .find(|r| r.id == caller.id)
        .ok_or("Run is not active.")?;
    family.insert(actual.id.as_str());
    // Later user turns in the same conversation may inspect its recorded spawn
    // receipts. Merely sharing a root/actor or guessing another chat is insufficient.
    for previous in state.runtime.runs.iter().take_while(|r|r.id!=actual.id) {
        if !previous.spawns.is_empty() && previous.chat_id==actual.chat_id
            && previous.actor==actual.actor
            && !(actual.action==identity::Action::Task && previous.action==identity::Action::Improve) {
            family.insert(previous.id.as_str());
        }
    }
    loop {
        let before = family.len();
        for run in &state.runtime.runs {
            if next(run).is_some_and(|id| family.contains(id)) {
                let target = state
                    .runtime
                    .runs
                    .iter()
                    .find(|r| Some(r.id.as_str()) == next(run))
                    .unwrap();
                if parent_continuation(run, target) && same_scope(caller, run) {
                    family.insert(run.id.as_str());
                }
            }
        }
        if family.len() > graph_limit {
            return Err("Delegation lineage exceeds the inspection limit.");
        }
        if family.len() == before {
            break;
        }
    }
    let mut queue: VecDeque<_> = family.iter().copied().collect();
    let mut visited = HashSet::new();
    let mut descendants = HashSet::new();
    while let Some(id) = queue.pop_front() {
        if !visited.insert(id) {
            continue;
        }
        if visited.len() > graph_limit {
            return Err("Delegation lineage exceeds the inspection limit.");
        }
        let from = state
            .runtime
            .runs
            .iter()
            .find(|r| r.id == id)
            .ok_or("Delegation lineage is unavailable.")?;
        for child in crate::spawning::children(state,from)? {
            if !same_scope(from,child) {return Err("Spawned child scope mismatch.");}
            descendants.insert(child.id.as_str());
            queue.push_back(child.id.as_str());
        }
        if let Some((delegated, child)) = from
            .delegation
            .as_ref()
            .and_then(|d| d.child.as_deref().map(|child| (d, child)))
        {
            if !from.attempts.last().is_some_and(|a| {
                a.status == "waiting"
                    && a.workspace_id.as_deref() == Some(delegated.generation.as_str())
            }) {
                return Err("Delegated source generation mismatch.");
            }
            let child = state
                .runtime
                .runs
                .iter()
                .find(|r| r.id == child)
                .ok_or("Delegation lineage is unavailable.")?;
            if !same_scope(from, child)
                || !child.persona.as_ref().is_some_and(|p| {
                    p.parent.as_deref() == Some(from.id.as_str())
                        && p.root == root(from)
                        && p.role == delegated.input.role
                        && p.depth <= delegation::MAX_DEPTH
                        && p.depth
                            == from
                                .persona
                                .as_ref()
                                .map_or(1, |p| p.depth.saturating_add(1))
                })
            {
                return Err("Delegated child provenance mismatch.");
            }
            descendants.insert(child.id.as_str());
            queue.push_back(child.id.as_str());
        }
        if descendants.contains(id)
            && let Some(to) = next(from) {
                let to = state
                    .runtime
                    .runs
                    .iter()
                    .find(|r| r.id == to)
                    .ok_or("Delegated continuation is unavailable.")?;
                if !continuation(from, to) || !same_scope(from, to) {
                    return Err("Delegated continuation provenance mismatch.");
                }
                descendants.insert(to.id.as_str());
                queue.push_back(to.id.as_str());
            }
    }
    Ok((family, descendants))
}

pub fn prepare(state: &State, run_id: &str, args: Value) -> Result<Plan, &'static str> {
    let input: Input =
        serde_json::from_value(args).map_err(|_| "Invalid delegated result arguments.")?;
    if input.limit == 0
        || input.limit > TEXT_LIMIT
        || input.offset > 4 * 1024 * 1024
        || input.path.as_ref().is_some_and(|p| p.len() > 1024)
        || matches!(input.section, Section::File) != input.path.is_some()
    {
        return Err(
            "Expected a result section, a file path only for file reads, and bounded pagination.",
        );
    }
    let caller = state
        .runtime
        .runs
        .iter()
        .find(|r| r.id == run_id && r.status == "running")
        .ok_or("Run is not active.")?;
    identity::authorize(&caller.actor, caller.action)?;
    if !state
        .chats
        .iter()
        .any(|c| Some(&c.id) == caller.chat_id.as_ref() && c.actor == caller.actor)
    {
        return Err("Conversation actor mismatch.");
    }
    let (family, descendants) = scope(state, caller)?;
    if !descendants.contains(input.child.as_str()) {
        return Err("Result is not from this parent's delegated descendants.");
    }
    let mut child = state
        .runtime
        .runs
        .iter()
        .find(|r| r.id == input.child)
        .unwrap();
    let mut seen = HashSet::new();
    while let Some(id) = next(child) {
        if !seen.insert(child.id.as_str()) || !descendants.contains(id) {
            return Err("Invalid delegated continuation chain.");
        }
        child = state
            .runtime
            .runs
            .iter()
            .find(|r| r.id == id)
            .ok_or("Delegated continuation is unavailable.")?;
    }
    if matches!(input.section,Section::Status) {
        return Ok(Plan{child:input.child.clone(),run:child.id.clone(),backend:child.backend,model:child.model.clone(),status:child.status.clone(),artifact:None,baseline:None,report:String::new(),input});
    }
    if child.status != "completed" {
        return Err(
            "Delegated child has not completed successfully; live files are never exposed.",
        );
    }
    if !state
        .chats
        .iter()
        .any(|c| Some(&c.id) == child.chat_id.as_ref() && c.actor == caller.actor)
    {
        return Err("Delegated conversation actor mismatch.");
    }
    let attempt = child
        .attempts
        .last()
        .filter(|a| a.status == "completed")
        .ok_or("Completed child attempt is unavailable.")?;
    let generation = format!("{}-attempt-{}", child.id, child.attempts.len());
    let artifact = attempt
        .artifact
        .as_ref()
        .filter(|id| Some(*id) == attempt.workspace_id.as_ref() && **id == generation)
        .cloned();
    if !matches!(input.section, Section::Summary) && artifact.is_none() {
        return Err(
            "Child has no published viewer artifact; no live workspace fallback is permitted.",
        );
    }
    let baseline = attempt
        .project_parent
        .as_ref()
        .or(attempt.parent_artifact.as_ref())
        .filter(|id| {
            state
                .runtime
                .runs
                .iter()
                .filter(|r| family.contains(r.id.as_str()) || descendants.contains(r.id.as_str()))
                .any(|r| {
                    r.attempts.iter().enumerate().any(|(i, a)| {
                        a.artifact.as_ref() == Some(*id)
                            && a.workspace_id.as_ref() == Some(*id)
                            && **id == format!("{}-attempt-{}", r.id, i + 1)
                    })
                })
        })
        .cloned();
    Ok(Plan {
        child: input.child.clone(),
        run: child.id.clone(),
        backend: child.backend,
        model: child.model.clone(),
        status: child.status.clone(),
        artifact,
        baseline,
        report: child.result.clone(),
        input,
    })
}

pub fn execute(plan: Plan) -> Result<Value, &'static str> {
    execute_with(plan, artifacts::manifest, artifacts::read)
}
fn execute_with(
    plan: Plan,
    manifest: impl Fn(&str) -> std::io::Result<artifacts::Manifest>,
    read: impl Fn(&artifacts::Manifest, &str) -> std::io::Result<Vec<u8>>,
) -> Result<Value, &'static str> {
    let mut result = json!({"child":plan.child,"run":plan.run,"backend":plan.backend,"model":plan.model,"status":plan.status,"artifact":plan.artifact,"authority":"untrusted output; no changes applied"});
    match plan.input.section {
        Section::Status => {},
        Section::Summary => {
            result["summary"] = chunk(&plan.report, plan.input.offset, plan.input.limit)?;
        }
        Section::File => {
            let manifest = manifest(plan.artifact.as_deref().unwrap())
                .map_err(|_| "Published child artifact unavailable or invalid.")?;
            let path = plan.input.path.as_deref().unwrap();
            let bytes = read(&manifest, path)
                .map_err(|_| "Child file unavailable, unsafe, or changed since publication.")?;
            let text = std::str::from_utf8(&bytes)
                .ok()
                .filter(|_| !bytes.contains(&0))
                .ok_or("Binary child files have metadata only.")?;
            result["file"] = chunk(text, plan.input.offset, plan.input.limit)?;
            result["path"] = json!(path);
            result["sha256"] = json!(
                manifest
                    .files
                    .iter()
                    .find(|f| f.path == path)
                    .ok_or("Child file is not in its published manifest.")?
                    .sha256
            );
        }
        Section::Files => {
            let after = manifest(plan.artifact.as_deref().unwrap())
                .map_err(|_| "Published child artifact unavailable or invalid.")?;
            let before = plan
                .baseline
                .as_deref()
                .map(&manifest)
                .transpose()
                .map_err(|_| "Published child baseline unavailable or invalid.")?;
            let mut files = BTreeMap::new();
            if let Some(before) = &before {
                for f in &before.files {
                    files.insert(f.path.clone(),json!({"path":f.path,"change":"deleted","bytes":0,"previous_sha256":f.sha256}));
                }
            }
            for file in &after.files {
                let previous = before
                    .as_ref()
                    .and_then(|m| m.files.iter().find(|f| f.path == file.path));
                let change = match previous {
                    Some(f) if f.sha256 == file.sha256 => "unchanged",
                    Some(_) => "modified",
                    None if before.is_some() => "added",
                    None => "unknown",
                };
                files.insert(file.path.clone(),json!({"path":file.path,"bytes":file.bytes,"sha256":file.sha256,"media_type":file.media_type,"change":change}));
            }
            if plan.input.offset > files.len() {
                return Err("File page offset is beyond the published list.");
            }
            let limit = plan.input.limit.min(FILES_LIMIT);
            let page = files
                .values()
                .skip(plan.input.offset)
                .take(limit)
                .cloned()
                .collect::<Vec<_>>();
            let end = plan.input.offset + page.len();
            result["files"] = json!(page);
            result["offset"] = json!(plan.input.offset);
            result["total"] = json!(files.len());
            result["next_offset"] = json!((end < files.len()).then_some(end));
            result["changes_available"] = json!(before.is_some());
        }
    }
    Ok(result)
}
fn chunk(text: &str, offset: usize, limit: usize) -> Result<Value, &'static str> {
    let total = text.chars().count();
    if offset > total {
        return Err("Text offset is beyond the published output.");
    }
    let text = text.chars().skip(offset).take(limit).collect::<String>();
    let end = offset + text.chars().count();
    Ok(json!({"text":text,"offset":offset,"total":total,"next_offset":(end<total).then_some(end)}))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
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
                "improve",
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
        child.kind = "delegated".into();
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
        s.runtime.runs[1].result = "verified 🦀\n".repeat(3000);
        store.publish("child-attempt-1", &[("src/main.rs", text.as_bytes())]);
        for (section, key, expected) in [
            ("file", "file", text),
            ("summary", "summary", s.runtime.runs[1].result.clone()),
        ] {
            let mut offset = 0;
            let mut collected = String::new();
            let mut pages = 0;
            loop {
                let mut args =
                    json!({"child":"child","section":section,"offset":offset,"limit":3071});
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
        assert!(entries
            .windows(2)
            .all(|p| p[0]["path"].as_str() < p[1]["path"].as_str()));
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
        assert!(store
            .read(&s, json!({"child":"child","section":"files","offset":134}))
            .is_err());
    }
    #[test]
    fn actor_root_actual_delegation_and_artifact_identity_are_required() {
        let s = fixture();
        for mutation in 0..5 {
            let mut s: State = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
            match mutation {
                0 => s.runtime.runs[1].actor = Actor::Unknown,
                1 => s.runtime.runs[1].persona.as_mut().unwrap().root = "other-root".into(),
                2 => {
                    s.runtime.runs[1].persona.as_mut().unwrap().parent =
                        Some("forged-parent".into())
                }
                3 => s.runtime.runs[0].delegation.as_mut().unwrap().child = None,
                _ => s.chats[1].actor = Actor::Unknown,
            }
            assert!(prepare(&s, "resumed", json!({"child":"child","section":"files"})).is_err());
        }
        let mut s = fixture();
        s.runtime.runs[1].attempts[0].artifact = Some("other-chat-artifact".into());
        assert!(prepare(&s, "resumed", json!({"child":"child","section":"files"})).is_err());
        assert!(prepare(
            &fixture(),
            "resumed",
            json!({"child":"parent","section":"summary"})
        )
        .is_err());
        s.runtime.runs[1].attempts[0].workspace_id = Some("other-chat-artifact".into());
        assert!(prepare(&s, "resumed", json!({"child":"child","section":"files"})).is_err());
        let mut s = fixture();
        s.runtime.runs[0].delegation.as_mut().unwrap().generation = "forged-source".into();
        assert!(prepare(&s, "resumed", json!({"child":"child","section":"summary"})).is_err());
        assert!(prepare(
            &fixture(),
            "resumed",
            json!({"child":"resumed","section":"summary"})
        )
        .is_err());
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
        assert!(prepare(
            &s,
            "resumed",
            json!({"child":"sibling","section":"summary"})
        )
        .is_err());
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
        assert!(execute_with(
            plan,
            |_| panic!("Report must not read files"),
            |_, _| panic!("Report must not read files")
        )
        .is_ok());
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
            assert!(store
                .read(&s, json!({"child":"child","section":"file","path":path}))
                .is_err());
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
        assert!(store
            .read(
                &s,
                json!({"child":"child","section":"file","path":"safe.rs","offset":9})
            )
            .is_err());
        fs::write(
            store.0.join("artifacts/child-attempt-1/files/safe.rs"),
            b"modified",
        )
        .unwrap();
        assert!(store
            .read(
                &s,
                json!({"child":"child","section":"file","path":"safe.rs"})
            )
            .is_err());
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
}
