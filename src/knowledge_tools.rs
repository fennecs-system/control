//! Bounded knowledge projections and checkpointed proposals. Only the operator
//! review surface can admit a proposal; a worker never edits accepted records.
use crate::runtime::RunStatus;
use crate::{
    State,
    collective::{Knowledge, ProposalStatus},
    identity::{self, Actor},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io;

pub(crate) const MAX_PROPOSALS: usize = 256;
#[derive(Clone, Serialize, Deserialize)]
pub struct Source {
    pub actor: Actor,
    pub chat: String,
    pub run: Option<String>,
    pub attempt: Option<String>,
    pub nonce: String,
    pub lenses: Vec<control_context::LensVersion>,
    pub documents: Vec<(String, u64)>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    claim: String,
    evidence: String,
    scope: String,
    validity: String,
    chat: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    query: String,
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    id: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "control.knowledge.search",
            "description": concat!(
                "Search admitted claims and documents. Workers see their captured lens documents; ordinary notes are ",
                "not verified facts. At most 10 concise results."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "minLength": 1, "maxLength": 200},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 10}
                },
                "required": ["query"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true}
        }),
        json!({
            "name": "control.knowledge.read",
            "description": concat!(
                "Read an admitted claim or selected document by search result ID. Text is paged in Unicode ",
                "characters, at most 4000 per call. Notes are not verified facts."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "maxLength": 200},
                    "offset": {"type": "integer", "minimum": 0},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 4000}
                },
                "required": ["id"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true}
        }),
        json!({
            "name": "control.knowledge.propose",
            "description": concat!(
                "Propose one concise claim with evidence, scope and recheck condition. Propose only a verified ",
                "finding other tasks can reuse. Pending until the operator reviews it; returns at once, so carry on. ",
                "Does not edit documents or accepted facts. Local MCP callers supply their own local operator chat ",
                "ID; workers use their bound chat."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "claim": {"type": "string", "minLength": 1, "maxLength": 280},
                    "evidence": {"type": "string", "minLength": 1, "maxLength": 1000},
                    "scope": {"type": "string", "minLength": 1, "maxLength": 300},
                    "validity": {"type": "string", "minLength": 1, "maxLength": 300},
                    "chat": {"type": "string", "maxLength": 128}
                },
                "required": ["claim", "evidence", "scope", "validity"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false}
        }),
    ]
}
fn canonical(name: &str) -> &str {
    match name {
        "control_knowledge_search" => "control.knowledge.search",
        "control_knowledge_read" => "control.knowledge.read",
        "control_knowledge_propose" => "control.knowledge.propose",
        other => other,
    }
}
/// The knowledge tools that only read.
pub fn reads(name: &str) -> bool {
    matches!(canonical(name), "control.knowledge.search" | "control.knowledge.read")
}
pub fn known(name: &str) -> bool {
    matches!(
        canonical(name),
        "control.knowledge.search" | "control.knowledge.read" | "control.knowledge.propose"
    )
}

struct Record<'a> {
    id: String,
    title: &'a str,
    body: &'a str,
    revision: Option<u64>,
    admitted: bool,
}
fn records<'a>(
    documents: &'a [crate::knowledge::Document],
    proposals: &'a [Knowledge],
    run: Option<&'a crate::runtime::Run>,
) -> impl Iterator<Item = Record<'a>> {
    let captured = run
        .and_then(|run| run.context.as_ref())
        .into_iter()
        .flat_map(|context| context.documents.iter())
        .map(|doc| Record {
            id: format!("document:{}", doc.id),
            title: &doc.title,
            body: &doc.body,
            revision: Some(doc.revision),
            admitted: false,
        });
    let local = documents.iter().filter(move |_| run.is_none()).map(|doc| Record {
        id: format!("document:{}", doc.id),
        title: &doc.title,
        body: &doc.body,
        revision: Some(doc.revision),
        admitted: false,
    });
    let accepted = proposals
        .iter()
        .enumerate()
        .filter(|(_, p)| p.status == ProposalStatus::Accepted)
        .map(|(index, p)| Record {
            id: format!("claim:{index}"),
            title: &p.claim,
            body: &p.claim,
            revision: None,
            admitted: true,
        });
    captured.chain(local).chain(accepted)
}
pub fn read(
    documents: &[crate::knowledge::Document],
    proposals: &[Knowledge],
    run: Option<&crate::runtime::Run>,
    name: &str,
    args: &Value,
) -> Result<Value, &'static str> {
    match canonical(name) {
        "control.knowledge.search" => {
            let input: Search =
                serde_json::from_value(args.clone()).map_err(|_| "Expected query and optional limit")?;
            let limit = input.limit.unwrap_or(10);
            if input.query.trim().is_empty() || input.query.chars().count() > 200 || !(1..=10).contains(&limit) {
                return Err("Use a query of 1–200 characters and limit of 1–10");
            }
            let query = input.query.to_lowercase();
            let terms = query.split_whitespace().collect::<Vec<_>>();
            let results=records(documents,proposals,run).filter(|record| {
                let title=record.title.to_lowercase();let body=record.body.to_lowercase();
                terms.iter().all(|term|title.contains(term)||body.contains(term))
            }).take(limit).map(|record|json!({"id":record.id,"title":record.title.chars().take(280).collect::<String>(),"excerpt":record.body.chars().take(180).collect::<String>(),"revision":record.revision,"admitted":record.admitted})).collect::<Vec<_>>();
            Ok(json!({"results":results}))
        }
        "control.knowledge.read" => {
            let input: Read =
                serde_json::from_value(args.clone()).map_err(|_| "Expected id and optional offset/limit")?;
            let offset = input.offset.unwrap_or(0);
            let limit = input.limit.unwrap_or(4000);
            if input.id.len() > 200 || offset > 32000 || !(1..=4000).contains(&limit) {
                return Err("Use an ID, character offset up to 32000 and limit of 1–4000");
            }
            let record = records(documents, proposals, run)
                .find(|record| record.id == input.id)
                .ok_or("Knowledge entry not found in this context")?;
            let mut chars = record.body.chars().skip(offset);
            let body = chars.by_ref().take(limit).collect::<String>();
            let next = chars.next().map(|_| offset + body.chars().count());
            let mut result = json!({"id":record.id,"title":record.title.chars().take(280).collect::<String>(),"body":body,"revision":record.revision,"admitted":record.admitted,"next_offset":next});
            if let Some(index) = input.id.strip_prefix("claim:").and_then(|id| id.parse::<usize>().ok()) {
                let proposal = &proposals[index];
                result["evidence"] = json!(proposal.evidence.chars().take(1000).collect::<String>());
                result["scope"] = json!(proposal.scope.chars().take(300).collect::<String>());
                result["validity"] = json!(proposal.validity.chars().take(300).collect::<String>());
                result["source"]=json!(proposal.source.as_ref().map(|source|json!({"actor":source.actor,"chat":source.chat,"run":source.run,"attempt":source.attempt,"lens_count":source.lenses.len(),"document_count":source.documents.len()})));
            }
            Ok(result)
        }
        _ => Err("Unknown knowledge read tool"),
    }
}
fn propose(
    state: &mut State,
    args: &Value,
    source: Source,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> Result<Value, &'static str> {
    let mut input: Proposal =
        serde_json::from_value(args.clone()).map_err(|_| "Expected claim, evidence, scope and validity")?;
    for (text, limit) in [
        (&mut input.claim, 280),
        (&mut input.evidence, 1000),
        (&mut input.scope, 300),
        (&mut input.validity, 300),
    ] {
        *text = text.trim().to_owned();
        if text.is_empty()
            || text.chars().count() > limit
            || text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err("Keep claim/evidence/scope/recheck within 280/1000/300/300 characters");
        }
    }
    if input.chat.as_ref().is_some_and(|chat| chat != &source.chat) {
        return Err("Proposal chat must match the caller");
    }
    let existing = state.collective.proposals.iter().enumerate().find(|(_, p)| {
        p.source.as_ref().is_some_and(|s| {
            s.run == source.run && s.attempt == source.attempt && s.chat == source.chat && s.nonce == source.nonce
        })
    });
    if let Some((index, previous)) = existing {
        if previous.claim != input.claim
            || previous.evidence != input.evidence
            || previous.scope != input.scope
            || previous.validity != input.validity
        {
            return Err("Request identity already used for another proposal");
        }
        return Ok(json!({"id":format!("claim:{index}"),"status":previous.status,"review":"/knowledge?mode=review"}));
    }
    if state.collective.proposals.len() >= MAX_PROPOSALS {
        return Err("Knowledge proposal capacity reached; operator maintenance required");
    }
    if state
        .collective
        .proposals
        .iter()
        .any(|p| p.status.is_standing() && p.claim.eq_ignore_ascii_case(&input.claim))
    {
        return Err("That claim already exists; review the existing entry");
    }
    let index = state.collective.proposals.len();
    state.collective.proposals.push(crate::collective::Knowledge {
        claim: input.claim,
        evidence: input.evidence,
        scope: input.scope,
        validity: input.validity,
        status: ProposalStatus::Pending,
        source: Some(source),
    });
    if save(state).is_err() {
        state.collective.proposals.pop();
        return Err("Could not save knowledge proposal");
    }
    Ok(json!({"id":format!("claim:{index}"),"status":"pending","review":"/knowledge?mode=review"}))
}
pub fn worker(
    state: &mut State,
    run_id: &str,
    name: &str,
    args: &Value,
    nonce: &str,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> Result<Value, &'static str> {
    let run = state
        .runtime
        .runs
        .get(run_id)
        .filter(|run| run.status == RunStatus::Running)
        .ok_or("Run is not active")?;
    identity::authorize(&run.actor, run.action)?;
    if crate::delegation::is_judge(run) && canonical(name) == "control.knowledge.propose" {
        return Err("Judges may only read knowledge");
    }
    if canonical(name) != "control.knowledge.propose" {
        return read(&state.documents, &state.collective.proposals, Some(run), name, args);
    }
    if nonce.is_empty() || nonce.len() > 128 || !nonce.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err("Missing or invalid request identity");
    }
    let context = run.context.as_ref();
    let source = Source {
        actor: run.actor.clone(),
        chat: run.chat_id.clone().ok_or("Run has no chat")?,
        run: Some(run.id().as_str().to_owned()),
        attempt: Some(
            run.attempts
                .last()
                .and_then(|attempt| attempt.workspace_id.clone())
                .ok_or("Run has no active attempt")?,
        ),
        nonce: nonce.into(),
        lenses: context.map(|c| c.lenses.clone()).unwrap_or_default(),
        documents: context
            .into_iter()
            .flat_map(|c| c.documents.iter().map(|d| (d.id.clone(), d.revision)))
            .collect(),
    };
    propose(state, args, source, save)
}
pub fn local(
    state: &mut State,
    name: &str,
    args: &Value,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> Result<Value, &'static str> {
    if canonical(name) != "control.knowledge.propose" {
        return read(&state.documents, &state.collective.proposals, None, name, args);
    }
    let chat = args["chat"]
        .as_str()
        .ok_or("Local proposal requires a local operator chat ID")?;
    let session = state
        .chats
        .iter()
        .find(|session| session.id == chat && session.actor == Actor::LocalOperator)
        .ok_or("Local operator chat not found")?;
    let bytes = serde_json::to_vec(args).map_err(|_| "Invalid proposal")?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    let nonce = digest.as_ref().iter().map(|byte| format!("{byte:02x}")).collect();
    let source = Source {
        actor: Actor::LocalOperator,
        chat: session.id.clone(),
        run: None,
        attempt: None,
        nonce,
        lenses: Vec::new(),
        documents: Vec::new(),
    };
    propose(state, args, source, save)
}

/// Pending worker proposals from `chat` and the chats spawned below it, as
/// operator requests (`pending_knowledge`). Decided only through [`review_action`].
pub(crate) fn pending_json(proposals: &[Knowledge], lineage: crate::chat::Lineage<'_>, chat: &str) -> Vec<Value> {
    proposals.iter().enumerate().filter(|(_, p)| p.status == ProposalStatus::Pending)
        .filter_map(|(index, p)| p.source.as_ref().filter(|s| lineage.within(&s.chat, chat)).map(|s| (index, p, s)))
        .map(|(index, p, s)| json!({"claim_id":format!("claim:{index}"),"chat_id":s.chat,"claim":p.claim,"evidence":p.evidence,"scope":p.scope,"validity":p.validity,
            "summary":format!("{}\nEvidence: {}\nScope: {}\nRecheck when: {}", p.claim, p.evidence, p.scope, p.validity)}))
        .collect()
}
/// Chats with a pending proposal, for the node-wide request view.
pub(crate) fn pending_chats(proposals: &[Knowledge]) -> impl Iterator<Item = &str> {
    proposals
        .iter()
        .filter(|p| p.status == ProposalStatus::Pending)
        .filter_map(|p| p.source.as_ref().map(|s| s.chat.as_str()))
}
/// The in-chat request panel: the same `/review` form as Knowledge → Review.
pub(crate) fn pending_panel(pending: &[Value]) -> String {
    pending.iter().map(|p| {
        let text = |key: &str| crate::escape(p[key].as_str().unwrap_or(""));
        format!("<article class='assistant-message pending-decision'><p class='message-status'>Knowledge proposal · <a target='_top' href='/knowledge?mode=review'>Review ↗</a></p><h3>{}</h3><dl><dt>Evidence</dt><dd>{}</dd><dt>Scope</dt><dd>{}</dd><dt>Recheck when</dt><dd>{}</dd></dl><form class='actions' method='post' action='/review' target='_top'><input type='hidden' name='id' value='{}'><button class='primary' name='decision' value='accepted'>Accept entry</button><button name='decision' value='rejected'>Reject</button></form></article>",
            text("claim"), text("evidence"), text("scope"), text("validity"), text("claim_id").trim_start_matches("claim:"))
    }).collect()
}

/// Why a review of a proposal that is not pending (or an invalid decision) is refused.
pub(crate) const STALE: &str = "That proposal is no longer pending; see Knowledge → Review.";
/// Local web admission is checkpointed separately from ordinary chat mutations.
/// Knowledge → Review and request replies both decide here; a proposal that is
/// no longer pending is refused.
pub fn review_action(
    state: &mut State,
    path: &str,
    fields: &std::collections::HashMap<String, String>,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> Result<(), &'static str> {
    if path == "/review" {
        let pending = fields
            .get("id")
            .and_then(|id| id.parse::<usize>().ok())
            .and_then(|index| state.collective.proposals.get(index))
            .is_some_and(|p| p.status == ProposalStatus::Pending);
        if !pending
            || !fields
                .get("decision")
                .is_some_and(|d| ProposalStatus::parse(d).is_decision())
        {
            return Err(STALE);
        }
    }
    let count = state.collective.proposals.len();
    let old = fields
        .get("id")
        .and_then(|id| id.parse::<usize>().ok())
        .and_then(|index| {
            state
                .collective
                .proposals
                .get(index)
                .map(|proposal| (index, proposal.status.clone()))
        });
    let notice = state.collective.notice.clone();
    state.collective.action(path, fields);
    if save(state).is_err() {
        state.collective.proposals.truncate(count);
        if let Some((index, status)) = old {
            state.collective.proposals[index].status = status;
        }
        state.collective.notice = notice;
        return Err("Could not persist the knowledge change");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
