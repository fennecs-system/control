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
        report: child.result.to_string(),
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
pub(crate) mod tests;
