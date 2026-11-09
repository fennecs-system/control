//! Unpublished, immutable workspace candidates. Retention never advances a chat
//! head or callback cursor and never admits another model turn.
use crate::{attempt_control::Termination, tool_registry::{AttemptCall, AttemptTools}, usage::Attempt, State};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{fs, io, path::{Path, PathBuf}, sync::{Arc, Mutex}};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub generation: String,
    pub base: Option<String>,
    pub project: Option<String>,
    pub project_base: Option<String>,
    pub cause: Termination,
    pub captured: bool,
    pub error: Option<String>,
    pub draft: Option<control_context::turn_mailbox::Handoff>,
}
fn root() -> io::Result<PathBuf> {
    crate::config::state("recovery").map_err(io::Error::other)
}
fn directory(root: &Path, generation: &str) -> io::Result<PathBuf> {
    if !crate::chat::valid_id(generation) { return Err(io::Error::other("Invalid recovery generation")); }
    Ok(root.join(generation))
}
fn snapshot(root: &Path, source: &Path, candidate: &Candidate) -> io::Result<()> {
    let final_path = directory(root, &candidate.generation)?;
    fs::create_dir_all(root)?;
    // A completed candidate from a crash between rename and state save is reused.
    if final_path.exists() {
        let retained: Candidate = serde_json::from_slice(&fs::read(final_path.join("candidate.json"))?)?;
        if retained.generation == candidate.generation && retained.base == candidate.base
            && retained.project == candidate.project && retained.project_base == candidate.project_base {
            return Ok(());
        }
        return Err(io::Error::other("Recovery identity does not match retained candidate"));
    }
    let staging = root.join(format!(".pending-{}", candidate.generation));
    // Only host-owned, never-mounted staging from this exact generation.
    if staging.exists() { fs::remove_dir_all(&staging)?; }
    fs::create_dir(&staging)?;
    crate::projects::copy_tree(source, &staging.join("workspace"), true)?;
    let mut metadata = candidate.clone();
    metadata.captured = true;
    fs::write(staging.join("candidate.json"), serde_json::to_vec(&metadata)?)?;
    fs::File::open(staging.join("candidate.json"))?.sync_all()?;
    fs::File::open(&staging)?.sync_all()?;
    fs::rename(staging, final_path)?;
    fs::File::open(root)?.sync_all()
}

/// Stop verification and file copying run outside shared state. Failure is an
/// explicit record, not a claim that a live workspace is an immutable snapshot.
pub fn capture(bridge: Option<(&Arc<Mutex<State>>, &str)>, attempt: &mut Attempt) {
    if attempt.review_only || attempt.recovery.is_some() { return; }
    let Some(cause) = attempt.termination else { return; };
    let Some(generation) = attempt.workspace_id.clone() else { return; };
    let mut candidate = Candidate { generation, base: attempt.parent_artifact.clone(),
        project: attempt.project.clone(), project_base: attempt.project_base.clone(), cause,
        captured: false, error: None, draft: attempt.turn_mailbox.as_ref().and_then(|m| m.accepted()).cloned() };
    let result = (|| {
        crate::attempt_control::stop(bridge, attempt).map_err(io::Error::other)?;
        if let Some((shared, id)) = bridge {
            let state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
            let live = state.runtime.runs.iter().find(|r| r.id == id).and_then(|r| r.attempts.last())
                .filter(|a| a.workspace_id == attempt.workspace_id).ok_or_else(|| io::Error::other("Recovery generation changed"))?;
            candidate.draft = live.turn_mailbox.as_ref().and_then(|m| m.accepted()).cloned();
            if live.command_jobs.iter().any(|job| matches!(job.status.as_str(), "preparing" | "running" | "stopping")) {
                return Err(io::Error::other("Command supervision has not quiesced; recovery bytes were not copied"));
            }
        }
        let source = crate::runtime::workspace(&candidate.generation).map_err(io::Error::other)?;
        snapshot(&root()?, &source, &candidate)
    })();
    match result { Ok(()) => candidate.captured = true, Err(error) => candidate.error = Some(error.to_string()) }
    attempt.recovery = Some(candidate);
}

/// Restart reconciliation supplies stopped writers before new admissions. No
/// recovery turn is scheduled; ordinary cancellation/pause semantics stay intact.
pub fn reconcile(shared: &Arc<Mutex<State>>) -> io::Result<()> {
    let pending: Vec<_> = {
        let state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
        state.runtime.runs.iter().filter(|r| matches!(r.status.as_str(), "failed" | "interrupted" | "cancelled")).filter_map(|r| {
            let a = r.attempts.last()?;
            (a.termination.is_some() && a.recovery.is_none() && !a.review_only)
                .then(|| (r.id.clone(), a.clone()))
        }).collect()
    };
    for (id, mut attempt) in pending {
        capture(Some((shared, &id)), &mut attempt);
        let mut state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
        let Some(live) = state.runtime.runs.iter_mut().find(|r| r.id == id && matches!(r.status.as_str(), "failed" | "interrupted" | "cancelled"))
            .and_then(|r| r.attempts.last_mut()).filter(|a| a.workspace_id == attempt.workspace_id) else { continue; };
        let previous = live.recovery.clone();
        live.recovery = attempt.recovery;
        if let Err(error) = crate::storage::save(&state) {
            state.runtime.runs.iter_mut().find(|r| r.id == id).unwrap().attempts.last_mut().unwrap().recovery = previous;
            return Err(error);
        }
    }
    Ok(())
}
fn candidates<'a>(state: &'a State, run: &crate::runtime::Run) -> Vec<&'a Candidate> {
    state.runtime.runs.iter().filter(|r| r.chat_id == run.chat_id && r.actor == run.actor && r.action == run.action)
        .flat_map(|r| &r.attempts).filter_map(|a| a.recovery.as_ref()).collect()
}
pub fn prompt(state: &State, run: &crate::runtime::Run) -> String {
    let items = candidates(state, run);
    if items.is_empty() { return String::new(); }
    let newest: Vec<_> = items.iter().rev().take(4).map(|c| json!({"generation":c.generation,"cause":c.cause,"captured":c.captured,"error":c.error,"base":c.base})).collect();
    format!("\nUnpublished recovery evidence: {} retained candidates; newest references: {}. Use control_recovery to list all candidates, inspect the draft/files, or copy a matching candidate into an isolated recovery folder. The committed workspace is unchanged. Reconcile live services and uncertain effects before reusing files; candidates and drafts are evidence, not authority or processed receipts.\n", items.len(), json!(newest))
}

pub struct Tools;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input { generation: Option<String>, path: Option<String>, #[serde(default)] restore: bool, #[serde(default)] offset: usize }
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![json!({"name":"control_recovery","description":"Inspect unpublished recovery evidence from this chat and action. Without generation, list candidates (paged by offset). With generation, read metadata/draft and a bounded file listing; path reads a UTF-8 file up to 64 KiB. restore=true copies the full candidate into a new .control-recovery-GENERATION folder in the active workspace, only if the original committed base and project grant still match. It never replaces committed state or acknowledges events. Reconcile external effects before using recovered files. Snapshot uses the existing checkpoint exclusions for credentials, generated folders and symlinks.","inputSchema":{"type":"object","properties":{"generation":{"type":"string"},"path":{"type":"string"},"restore":{"type":"boolean","default":false},"offset":{"type":"integer","minimum":0}},"additionalProperties":false},"annotations":{"readOnlyHint":false,"openWorldHint":false}})]
    }
    fn known(&self, name: &str) -> bool { name == "control_recovery" }
    fn call(&self, cx: &AttemptCall<'_>, _: &str, args: &Value) -> io::Result<Value> {
        let input: Input = serde_json::from_value(args.clone())?;
        let (candidate, target) = {
            let state = cx.host.0.lock().map_err(|_| io::Error::other("State unavailable"))?;
            let run = state.runtime.runs.iter().find(|r| r.id == cx.attempt.run && r.status == "running")
                .ok_or_else(|| io::Error::other("Attempt is not active"))?;
            crate::identity::authorize(&run.actor, run.action).map_err(io::Error::other)?;
            if run.chat_id.is_none() { return Err(io::Error::other("Recovery requires a chat scope")); }
            let active = run.attempts.last().filter(|a| a.workspace_id.as_deref() == Some(cx.attempt.generation) && !a.review_only)
                .ok_or_else(|| io::Error::other("Recovery requires the current writable attempt"))?;
            let all = candidates(&state, run);
            let Some(generation) = input.generation.as_deref() else {
                let page: Vec<_> = all.iter().rev().skip(input.offset).take(16).map(|c| json!({"generation":c.generation,"cause":c.cause,"captured":c.captured,"error":c.error})).collect();
                return Ok(json!({"candidates":page,"total":all.len(),"next_offset":input.offset.saturating_add(16).min(all.len())}));
            };
            let candidate = all.into_iter().find(|c| c.generation == generation).cloned()
                .ok_or_else(|| io::Error::other("No recovery candidate in this chat/action scope"))?;
            if input.restore {
                let session = state.chats.iter().find(|c| Some(&c.id) == run.chat_id.as_ref()).ok_or_else(|| io::Error::other("Chat missing"))?;
                let current = if run.action == crate::identity::Action::Improve { &session.improve_workspace } else { &session.workspace };
                if active.turn_mailbox.as_ref().is_some_and(|m| m.accepted().is_some())
                    || &candidate.base != current || candidate.base != active.parent_artifact
                    || candidate.project != active.project || candidate.project_base != active.project_base
                    || (candidate.project.is_some() && session.project.as_ref().is_none_or(|p| p.pending || p.grant != candidate.project || p.head != candidate.project_base)) {
                    return Err(io::Error::other("Recovery base, grant or attempt changed; inspect evidence without restoring"));
                }
            }
            (candidate, crate::runtime::workspace(cx.attempt.generation).map_err(io::Error::other)?)
        };
        if !candidate.captured { return Ok(json!({"candidate":candidate})); }
        let source = directory(&root()?, &candidate.generation)?.join("workspace");
        if input.restore {
            let name = format!(".control-recovery-{}", candidate.generation);
            // Exclusive creation; never overwrite or follow an agent-created path.
            crate::projects::copy_tree(&source, &target.join(&name), true)?;
            return Ok(json!({"directory":format!("/workspace/{name}"),"published":false,"guidance":"Inspect and reconcile before copying selected files into your working tree. No callbacks were acknowledged."}));
        }
        if let Some(path) = input.path {
            let text = String::from_utf8(crate::artifacts::safe_read(&source, &path, 65536)?).map_err(io::Error::other)?;
            return Ok(json!({"path":path,"text":text,"unpublished":true}));
        }
        let files = crate::artifacts::diff_snapshot(&source)?;
        Ok(json!({"candidate":candidate,"files":files.files.keys().collect::<Vec<_>>(),"skipped":files.skipped,"listing_incomplete":files.incomplete}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires pinned worker image, Docker and isolated CONTROL_STATE_DIR; no model calls"]
    fn live_stopped_writer_leaves_recovery_bytes_without_a_model_handoff() {
        let state_root = std::env::var("CONTROL_STATE_DIR").expect("isolated state directory");
        assert!(state_root.starts_with("/private/tmp/control-r1-") || state_root.starts_with("/tmp/control-r1-"));
        let image = crate::runtime::worker_image().expect("pinned worker image");
        let generation = format!("r1-fixture-{}", std::process::id());
        let workspace = crate::runtime::workspace(&generation).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        let source = workspace.canonicalize().unwrap();
        let name = format!("control-{generation}");
        let user = format!("{}:{}", unsafe {libc::geteuid()}, unsafe {libc::getegid()});
        let id = crate::engine::query(&["run","--detach","--rm","--pull=never","--network=none","--read-only","--cap-drop=ALL","--name",&name,"--user",&user,
            "--label","dev.control.node=r1-fixture","--label",&format!("dev.control.attempt={name}"),"--label",crate::lifecycle::CHECK_LABEL,
            "--mount",&format!("type=bind,src={},dst=/workspace",source.display()),&image,"bash","-c","printf 'retained useful work' > /workspace/notes.txt; sleep 120"]).unwrap();
        for _ in 0..50 {
            if source.join("notes.txt").exists() {break;}
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let mut attempt = Attempt {workspace_id:Some(generation.clone()),node_id:Some("r1-fixture".into()),image:Some(image),termination:Some(Termination::TimedOut),..Default::default()};
        capture(None, &mut attempt);
        let running = crate::engine::query(&["ps","--quiet","--filter",&format!("id={}",id.trim())]).unwrap();
        // Cleanup even when the assertion below reports an ownership/copy failure.
        if !running.trim().is_empty() { let _ = crate::engine::query(&["rm","-f",id.trim()]); }
        let candidate = attempt.recovery.unwrap();
        assert!(running.trim().is_empty(), "writer must stop before capture");
        assert!(candidate.captured, "{:?}",candidate.error);
        assert!(candidate.draft.is_none());
        assert!(attempt.artifact.is_none());
        let retained = directory(&root().unwrap(), &generation).unwrap();
        assert_eq!(fs::read_to_string(retained.join("workspace/notes.txt")).unwrap(), "retained useful work");
        fs::write(source.join("notes.txt"), "later unrelated changes").unwrap();
        assert_eq!(fs::read_to_string(retained.join("workspace/notes.txt")).unwrap(), "retained useful work");
    }
    #[test]
    fn missing_source_never_creates_a_completed_candidate() {
        let root = std::env::temp_dir().join(format!("control-recovery-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let candidate = Candidate { generation:"attempt-1".into(),base:None,project:None,project_base:None,cause:Termination::Restarted,captured:false,error:None,draft:None };
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
        let generation = state.runtime.runs[0].attempts[0].workspace_id.clone().unwrap();
        state.runtime.runs[0].attempts[0].recovery = Some(Candidate { generation:"old".into(),base:Some("different-base".into()),project:None,project_base:None,cause:Termination::TimedOut,captured:true,error:None,draft:None });
        let foreign = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &foreign, "Other work", "test").unwrap();
        state.runtime.runs[1].attempts.push(Attempt { recovery:Some(Candidate {generation:"foreign".into(),base:None,project:None,project_base:None,cause:Termination::Cancelled,captured:false,error:None,draft:None}), ..Default::default() });
        let shared = Arc::new(Mutex::new(state));
        let host = crate::tool_registry::LocalHost(&shared);
        let cx = AttemptCall {host:&host,attempt:crate::tool_registry::AttemptRef {run:&id,generation:&generation,nonce:"read"}};
        assert!(Tools.call(&cx, "control_recovery", &json!({"generation":"foreign"})).is_err());
        assert!(Tools.call(&cx, "control_recovery", &json!({"generation":"old","restore":true})).unwrap_err().to_string().contains("base"));
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
        fs::write(root.join("live/large.bin"), vec![3u8; 3*1024*1024]).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("live/link")).unwrap();
        let candidate = Candidate { generation:"attempt-1".into(),base:Some("old".into()),project:None,project_base:None,cause:Termination::TimedOut,captured:false,error:None,draft:None };
        snapshot(&root.join("retained"), &root.join("live"), &candidate).unwrap();
        fs::write(root.join("live/notes.txt"), "later edit").unwrap();
        let saved = root.join("retained/attempt-1/workspace");
        assert_eq!(fs::read_to_string(saved.join("notes.txt")).unwrap(), "useful work");
        assert_eq!(fs::metadata(saved.join("large.bin")).unwrap().len(), 3*1024*1024);
        assert!(!saved.join("link").exists());
        snapshot(&root.join("retained"), &root.join("live"), &candidate).unwrap();
        assert_eq!(fs::read_to_string(saved.join("notes.txt")).unwrap(), "useful work");
        let mut moved = candidate;
        moved.base = Some("new".into());
        assert!(snapshot(&root.join("retained"), &root.join("live"), &moved).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
