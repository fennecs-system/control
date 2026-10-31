//! Bounded knowledge projections and checkpointed proposals. Only the operator
//! review surface can admit a proposal; a worker never edits accepted records.
use crate::{
    State,
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
        json!({"name":"control.knowledge.search","description":"Search admitted claims and documents. Workers see their captured lens documents; ordinary notes are not verified facts. At most 10 concise results.","inputSchema":{"type":"object","properties":{"query":{"type":"string","minLength":1,"maxLength":200},"limit":{"type":"integer","minimum":1,"maximum":10}},"required":["query"],"additionalProperties":false},"annotations":{"readOnlyHint":true}}),
        json!({"name":"control.knowledge.read","description":"Read an admitted claim or selected document by search result ID. Text is paged in Unicode characters, at most 4000 per call. Notes are not verified facts.","inputSchema":{"type":"object","properties":{"id":{"type":"string","maxLength":200},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":4000}},"required":["id"],"additionalProperties":false},"annotations":{"readOnlyHint":true}}),
        json!({"name":"control.knowledge.propose","description":"Propose one concise claim with evidence, scope and recheck condition. Pending until a local operator reviews it in Knowledge → Review. Does not edit documents or accepted facts. Local MCP callers supply their own local operator chat ID; workers use their bound chat.","inputSchema":{"type":"object","properties":{"claim":{"type":"string","minLength":1,"maxLength":280},"evidence":{"type":"string","minLength":1,"maxLength":1000},"scope":{"type":"string","minLength":1,"maxLength":300},"validity":{"type":"string","minLength":1,"maxLength":300},"chat":{"type":"string","maxLength":128}},"required":["claim","evidence","scope","validity"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false}}),
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
    state: &'a State,
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
    let local = state
        .documents
        .iter()
        .filter(move |_| run.is_none())
        .map(|doc| Record {
            id: format!("document:{}", doc.id),
            title: &doc.title,
            body: &doc.body,
            revision: Some(doc.revision),
            admitted: false,
        });
    let accepted = state
        .collective
        .proposals
        .iter()
        .enumerate()
        .filter(|(_, p)| p.status == "accepted")
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
    state: &State,
    run: Option<&crate::runtime::Run>,
    name: &str,
    args: &Value,
) -> Result<Value, &'static str> {
    match canonical(name) {
        "control.knowledge.search" => {
            let input: Search = serde_json::from_value(args.clone())
                .map_err(|_| "Expected query and optional limit")?;
            let limit = input.limit.unwrap_or(10);
            if input.query.trim().is_empty()
                || input.query.chars().count() > 200
                || !(1..=10).contains(&limit)
            {
                return Err("Use a query of 1–200 characters and limit of 1–10");
            }
            let query = input.query.to_lowercase();
            let terms=query.split_whitespace().collect::<Vec<_>>();
            let results=records(state,run).filter(|record| {
                let title=record.title.to_lowercase();let body=record.body.to_lowercase();
                terms.iter().all(|term|title.contains(term)||body.contains(term))
            }).take(limit).map(|record|json!({"id":record.id,"title":record.title.chars().take(280).collect::<String>(),"excerpt":record.body.chars().take(180).collect::<String>(),"revision":record.revision,"admitted":record.admitted})).collect::<Vec<_>>();
            Ok(json!({"results":results}))
        }
        "control.knowledge.read" => {
            let input: Read = serde_json::from_value(args.clone())
                .map_err(|_| "Expected id and optional offset/limit")?;
            let offset = input.offset.unwrap_or(0);
            let limit = input.limit.unwrap_or(4000);
            if input.id.len() > 200 || offset > 32000 || !(1..=4000).contains(&limit) {
                return Err("Use an ID, character offset up to 32000 and limit of 1–4000");
            }
            let record = records(state, run)
                .find(|record| record.id == input.id)
                .ok_or("Knowledge entry not found in this context")?;
            let mut chars = record.body.chars().skip(offset);
            let body = chars.by_ref().take(limit).collect::<String>();
            let next = chars.next().map(|_| offset + body.chars().count());
            let mut result = json!({"id":record.id,"title":record.title.chars().take(280).collect::<String>(),"body":body,"revision":record.revision,"admitted":record.admitted,"next_offset":next});
            if let Some(index) = input
                .id
                .strip_prefix("claim:")
                .and_then(|id| id.parse::<usize>().ok())
            {
                let proposal = &state.collective.proposals[index];
                result["evidence"] =
                    json!(proposal.evidence.chars().take(1000).collect::<String>());
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
    let mut input: Proposal = serde_json::from_value(args.clone())
        .map_err(|_| "Expected claim, evidence, scope and validity")?;
    for (text, limit) in [
        (&mut input.claim, 280),
        (&mut input.evidence, 1000),
        (&mut input.scope, 300),
        (&mut input.validity, 300),
    ] {
        *text = text.trim().to_owned();
        if text.is_empty()
            || text.chars().count() > limit
            || text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err("Keep claim/evidence/scope/recheck within 280/1000/300/300 characters");
        }
    }
    if input.chat.as_ref().is_some_and(|chat| chat != &source.chat) {
        return Err("Proposal chat must match the caller");
    }
    let existing = state
        .collective
        .proposals
        .iter()
        .enumerate()
        .find(|(_, p)| {
            p.source.as_ref().is_some_and(|s| {
                s.run == source.run
                    && s.attempt == source.attempt
                    && s.chat == source.chat
                    && s.nonce == source.nonce
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
        return Ok(
            json!({"id":format!("claim:{index}"),"status":previous.status,"review":"/knowledge?mode=review"}),
        );
    }
    if state.collective.proposals.len() >= MAX_PROPOSALS {
        return Err("Knowledge proposal capacity reached; operator maintenance required");
    }
    if state
        .collective
        .proposals
        .iter()
        .any(|p| p.status != "rejected" && p.claim.eq_ignore_ascii_case(&input.claim))
    {
        return Err("That claim already exists; review the existing entry");
    }
    let index = state.collective.proposals.len();
    state
        .collective
        .proposals
        .push(crate::collective::Knowledge {
            claim: input.claim,
            evidence: input.evidence,
            scope: input.scope,
            validity: input.validity,
            status: "pending".into(),
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
        .iter()
        .find(|run| run.id == run_id && run.status == "running")
        .ok_or("Run is not active")?;
    identity::authorize(&run.actor, run.action)?;
    if crate::delegation::is_judge(run) && canonical(name) == "control.knowledge.propose" {
        return Err("Judges may only read knowledge");
    }
    if canonical(name) != "control.knowledge.propose" {
        return read(state, Some(run), name, args);
    }
    if nonce.is_empty()
        || nonce.len() > 128
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("Missing or invalid request identity");
    }
    let context = run.context.as_ref();
    let source = Source {
        actor: run.actor.clone(),
        chat: run.chat_id.clone().ok_or("Run has no chat")?,
        run: Some(run.id.clone()),
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
        return read(state, None, name, args);
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
    let nonce = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
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

/// Local web admission is checkpointed separately from ordinary chat mutations.
pub fn review_action(
    state: &mut State,
    path: &str,
    fields: &std::collections::HashMap<String, String>,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> Result<(), &'static str> {
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
mod tests {
    use super::*;
    fn fixture() -> (State, String) {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Check knowledge", "test").unwrap();
        let run = &mut state.runtime.runs[0];
        run.status = "running".into();
        run.attempts.push(crate::usage::Attempt {
            workspace_id: Some("knowledge-attempt".into()),
            ..Default::default()
        });
        run.context = Some(control_context::Resolved {
            documents: vec![control_context::DocumentInput {
                id: "selected".into(),
                title: "Selected.md".into(),
                revision: 2,
                body: "Old captured café 🐾".into(),
            }],
            ..Default::default()
        });
        let id = run.id.clone();
        state.documents.push(crate::knowledge::Document {
            id: "selected".into(),
            title: "Selected.md".into(),
            revision: 3,
            body: "New live body".into(),
            saved_at: None,
        });
        state.documents.push(crate::knowledge::Document {
            id: "unselected".into(),
            title: "Unselected.md".into(),
            revision: 1,
            body: "Private draft".into(),
            saved_at: None,
        });
        (state, id)
    }
    fn input() -> Value {
        json!({"claim":"A useful fact","evidence":"artifact:checked-result","scope":"fixture revision 2","validity":"recheck after source changes"})
    }
    #[test]
    fn captured_documents_are_immutable_and_unselected_notes_are_not_readable() {
        let (state, id) = fixture();
        let run = state.runtime.runs.iter().find(|run| run.id == id).unwrap();
        assert!(
            read(
                &state,
                Some(run),
                "control_knowledge_read",
                &json!({"id":"document:unselected"})
            )
            .is_err()
        );
        let first = read(
            &state,
            Some(run),
            "control_knowledge_read",
            &json!({"id":"document:selected","offset":13,"limit":5}),
        )
        .unwrap();
        assert_eq!(first["body"], "café ");
        assert_eq!(first["next_offset"], 18);
        assert_eq!(first["revision"], 2);
        assert_eq!(first["admitted"], false);
        let last = read(
            &state,
            Some(run),
            "control_knowledge_read",
            &json!({"id":"document:selected","offset":18}),
        )
        .unwrap();
        assert_eq!(last["body"], "🐾");
        assert!(last["next_offset"].is_null());
        assert_eq!(
            read(
                &state,
                None,
                "control.knowledge.read",
                &json!({"id":"document:selected"})
            )
            .unwrap()["body"],
            "New live body"
        );
        assert_eq!(
            read(
                &state,
                Some(run),
                "control.knowledge.search",
                &json!({"query":"live"})
            )
            .unwrap()["results"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }
    #[test]
    fn proposal_checkpoint_retries_provenance_and_operator_admission() {
        let (mut state, id) = fixture();
        let args = input();
        assert!(
            worker(
                &mut state,
                &id,
                "control_knowledge_propose",
                &args,
                "request-1",
                |_| Err(io::Error::other("full"))
            )
            .is_err()
        );
        assert!(state.collective.proposals.is_empty());
        let value = worker(
            &mut state,
            &id,
            "control_knowledge_propose",
            &args,
            "request-1",
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(value["status"], "pending");
        assert!(
            state
                .documents
                .iter()
                .all(|doc| doc.body != "A useful fact")
        );
        worker(
            &mut state,
            &id,
            "control_knowledge_propose",
            &args,
            "request-1",
            |_| panic!("Retry must not persist twice"),
        )
        .unwrap();
        assert_eq!(state.collective.proposals.len(), 1);
        let source = state.collective.proposals[0].source.as_ref().unwrap();
        assert_eq!(source.run.as_deref(), Some(id.as_str()));
        assert_eq!(source.attempt.as_deref(), Some("knowledge-attempt"));
        assert_eq!(source.documents, vec![("selected".into(), 2)]);
        let mut changed = args;
        changed["claim"] = json!("Another claim");
        assert!(
            worker(
                &mut state,
                &id,
                "control_knowledge_propose",
                &changed,
                "request-1",
                |_| Ok(())
            )
            .is_err()
        );
        assert!(
            read(
                &state,
                None,
                "control.knowledge.read",
                &json!({"id":"claim:0"})
            )
            .is_err()
        );
        let fields = [
            ("id".into(), "0".into()),
            ("decision".into(), "accepted".into()),
        ]
        .into_iter()
        .collect();
        assert!(
            review_action(&mut state, "/review", &fields, |_| Err(io::Error::other(
                "full"
            )))
            .is_err()
        );
        assert_eq!(state.collective.proposals[0].status, "pending");
        review_action(&mut state, "/review", &fields, |_| Ok(())).unwrap();
        let serialized = serde_json::to_vec(&state).unwrap();
        let restored: State = serde_json::from_slice(&serialized).unwrap();
        let accepted = read(
            &restored,
            None,
            "control.knowledge.read",
            &json!({"id":"claim:0"}),
        )
        .unwrap();
        assert_eq!(accepted["admitted"], true);
        assert_eq!(accepted["evidence"], "artifact:checked-result");
        assert_eq!(accepted["source"]["run"], id);
        assert!(
            crate::knowledge::render(&restored, "review", None).contains("artifact:checked-result")
        );
    }
    #[test]
    fn validation_bounds_and_authority_fail_closed() {
        let (mut state, id) = fixture();
        for args in [
            json!({"query":""}),
            json!({"query":"x","limit":11}),
            json!({"query":"x".repeat(201)}),
            json!({"query":"x","extra":true}),
        ] {
            assert!(read(&state, None, "control.knowledge.search", &args).is_err());
        }
        for args in [
            json!({"id":"document:selected","limit":4001}),
            json!({"id":"document:selected","offset":32001}),
        ] {
            assert!(read(&state, None, "control.knowledge.read", &args).is_err());
        }
        let mut args = input();
        args["claim"] = json!("x".repeat(281));
        assert!(
            worker(
                &mut state,
                &id,
                "control_knowledge_propose",
                &args,
                "one",
                |_| Ok(())
            )
            .is_err()
        );
        args = input();
        args["chat"] = json!("another-chat");
        assert!(
            worker(
                &mut state,
                &id,
                "control_knowledge_propose",
                &args,
                "one",
                |_| Ok(())
            )
            .is_err()
        );
        state.runtime.runs[0].actor = Actor::Unknown;
        assert!(
            worker(
                &mut state,
                &id,
                "control_knowledge_search",
                &json!({"query":"captured"}),
                "one",
                |_| Ok(())
            )
            .is_err()
        );
        state.runtime.runs[0].actor = Actor::LocalOperator;
        state.runtime.runs[0].status = "completed".into();
        assert!(
            worker(
                &mut state,
                &id,
                "control_knowledge_propose",
                &input(),
                "one",
                |_| Ok(())
            )
            .is_err()
        );
        assert!(state.collective.proposals.is_empty());
    }
    #[test]
    fn local_proposals_require_operator_chat_and_are_retry_safe() {
        let (mut state, _) = fixture();
        let chat = state.chats[0].id.clone();
        let mut args = input();
        assert!(local(&mut state, "control.knowledge.propose", &args, |_| Ok(())).is_err());
        args["chat"] = json!(chat);
        local(&mut state, "control.knowledge.propose", &args, |_| Ok(())).unwrap();
        local(&mut state, "control.knowledge.propose", &args, |_| {
            panic!("Duplicate persistence")
        })
        .unwrap();
        assert_eq!(state.collective.proposals.len(), 1);
        state.chats[0].actor = Actor::Unknown;
        assert!(local(&mut state, "control.knowledge.propose", &args, |_| Ok(())).is_err());
    }
    #[test]
    fn operator_mcp_routes_reads_and_advertises_all_knowledge_schemas() {
        let (mut state, _) = fixture();
        let listed = crate::session_http::rpc(
            &mut state,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        );
        let names = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        for name in [
            "control.knowledge.search",
            "control.knowledge.read",
            "control.knowledge.propose",
        ] {
            assert!(names.contains(&name));
        }
        let response = crate::session_http::rpc(
            &mut state,
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"control.knowledge.read","arguments":{"id":"document:selected"}}}),
        );
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["data"]["body"],
            "New live body"
        );
    }
    #[test]
    fn multiword_search_is_order_independent_and_requires_every_term() {
        let (mut state,_)=fixture();
        state.documents[0].title="Signal repeater.md".into();state.documents[0].body="The delay of a repeater is configurable. CAFÉ".into();
        for query in ["repeater delay","delay repeater","  REPEATER   delay  ","café repeater"] {
            let value=read(&state,None,"control.knowledge.search",&json!({"query":query})).unwrap();
            assert_eq!(value["results"].as_array().unwrap().len(),1,"{query}");assert_eq!(value["results"][0]["id"],"document:selected");
        }
        let missing=read(&state,None,"control.knowledge.search",&json!({"query":"repeater missing"})).unwrap();assert!(missing["results"].as_array().unwrap().is_empty());
    }
    #[test]
    fn search_result_count_and_proposal_storage_are_bounded() {
        let (mut state, id) = fixture();
        for index in 0..30 {
            state.documents.push(crate::knowledge::Document {
                id: format!("test-{index}"),
                title: "Match".into(),
                body: "match".repeat(1000),
                revision: 1,
                saved_at: None,
            });
        }
        let results = read(
            &state,
            None,
            "control.knowledge.search",
            &json!({"query":"match"}),
        )
        .unwrap();
        assert_eq!(results["results"].as_array().unwrap().len(), 10);
        assert!(results.to_string().len() < 4000);
        for index in 0..MAX_PROPOSALS {
            let mut args = input();
            args["claim"] = json!(format!("Fact {index}"));
            worker(
                &mut state,
                &id,
                "control_knowledge_propose",
                &args,
                &format!("request-{index}"),
                |_| Ok(()),
            )
            .unwrap();
        }
        assert!(
            worker(
                &mut state,
                &id,
                "control_knowledge_propose",
                &input(),
                "overflow",
                |_| Ok(())
            )
            .is_err()
        );
        assert_eq!(state.collective.proposals.len(), MAX_PROPOSALS);
    }
}
