//! `control_chat`: the chat MCP tools for Control's own agents, off unless the
//! operator enables "agents may use the chat tools" in Manage → Permissions.
//! An agent sees and changes only its own task: its chat and the chats spawned
//! below it. It writes only to those descendants (never its own chat or its
//! parents), creates DMs only as its own children, and never decides requests
//! or opens folders (it has `control_project_request` for that). Calls run the
//! operator endpoint's functions in process, one state lock per call.
use super::{Remote, call};
use crate::runtime::RunStatus;
use crate::store::Store;
use crate::{
    State,
    tool_registry::{AttemptCall, AttemptTools},
};
use serde_json::{Value, json};
use std::{collections::HashSet, io};

/// The chat tools an agent may call through `control_chat`.
pub const TOOLS: [&str; 10] = [
    "chats_list",
    "chat_read",
    "chat_team",
    "chat_requests",
    "chat_create",
    "chat_send",
    "chat_model",
    "chat_genes",
    "chat_cancel",
    "chat_stop_services",
];
/// Chats one task may hold before `chat_create` refuses.
const MAX_TEAM: usize = 32;
const READS: [&str; 4] = [
    "control.chats.list",
    "control.chat.read",
    "control.requests.list",
    "control.services.list",
];
const WRITES: [&str; 6] = [
    "control.chat.create",
    "control.chat.send",
    "control.chat.lenses",
    "control.chat.rename",
    "control.chat.persona",
    "control.services.stop",
];

/// The operator endpoint limited to `own`'s subtree.
pub(crate) struct Scoped<'a> {
    pub shared: &'a Store,
    pub own: String,
}
impl Scoped<'_> {
    fn team(&self, state: &State) -> HashSet<String> {
        crate::chat::Lineage::of(&state.chats)
            .subtree([self.own.as_str()])
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
    /// Whether this agent may call tool `name` with `arguments`; its team.
    fn admit(&self, state: &State, name: &str, arguments: &Value) -> io::Result<HashSet<String>> {
        let team = self.team(state);
        let write = WRITES.contains(&name);
        if !write && !READS.contains(&name) {
            return Err(io::Error::other("Not available to agents."));
        }
        if let Some(id) = arguments["id"].as_str() {
            if !team.contains(id) {
                return Err(io::Error::other(
                    "Only chats in your own task (your chat and the chats spawned below it).",
                ));
            }
            if write && id == self.own {
                return Err(io::Error::other(
                    "Agents change the DMs they spawned, not their own chat.",
                ));
            }
        }
        // Only the commands chat_model and chat_cancel build; never a decision.
        let command = arguments["message"]
            .as_str()
            .map(str::trim_start)
            .filter(|m| m.starts_with('/'));
        if command.is_some_and(|m| {
            !["/model", "/cancel"]
                .iter()
                .any(|c| m == *c || m.starts_with(&format!("{c} ")))
        }) {
            return Err(io::Error::other("Agents send plain text, /model or /cancel only."));
        }
        if name == "control.chat.create" && team.len() >= MAX_TEAM {
            return Err(io::Error::other(format!("This task already has {MAX_TEAM} chats.")));
        }
        Ok(team)
    }
}
impl Remote for Scoped<'_> {
    /// As the operator endpoint's `control.chat.send`, but under the lock
    /// it copies only the chat's tail, not the detail a server answers with.
    fn send(&mut self, id: &str, message: &str) -> io::Result<crate::chat::Tail> {
        let arguments = json!({"id":id,"message":message});
        self.shared.write(|state| {
            self.admit(state, "control.chat.send", &arguments)?;
            crate::session_http::send_with_tail(state, &arguments).map_err(|error| io::Error::other(error.into_owned()))
        })?
    }
    fn tool(&mut self, name: &str, arguments: Value) -> io::Result<Value> {
        self.shared.write(|state| {
            let team = self.admit(state, name, &arguments)?;
            let response = crate::session_http::rpc(
                state,
                &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
            );
            let result = &response["result"];
            if result["isError"] == true {
                return Err(io::Error::other(
                    result["content"][0]["text"].as_str().unwrap_or("Tool request failed."),
                ));
            }
            let mut data = result["structuredContent"]["data"].clone();
            match name {
                // A DM an agent creates is its own child, so it stays in its task.
                "control.chat.create" => {
                    let id = data["id"]
                        .as_str()
                        .ok_or_else(|| io::Error::other("Invalid chat response."))?
                        .to_owned();
                    let session = state
                        .chats
                        .iter_mut()
                        .find(|s| s.id == id)
                        .ok_or_else(|| io::Error::other("Chat not found."))?;
                    session.parent_chat = Some(self.own.clone());
                    session.revision = session.revision.saturating_add(1);
                    crate::storage::save(state)?;
                    data["parent_chat"] = json!(self.own);
                }
                "control.chats.list" => {
                    if let Some(rows) = data.as_array_mut() {
                        rows.retain(|row| row["id"].as_str().is_some_and(|id| team.contains(id)));
                    }
                }
                // One group for this task, holding only its own chats' requests.
                "control.requests.list" => {
                    let services = state
                        .services
                        .iter()
                        .filter(|s| team.contains(s.resource.owner.chat.as_str()))
                        .map(|s| s.resource.id.as_str().to_owned())
                        .collect::<HashSet<_>>();
                    let mine = |item: &Value| {
                        item["chat_id"].as_str().is_some_and(|c| team.contains(c))
                            || item["service_id"].as_str().is_some_and(|s| services.contains(s))
                    };
                    let mut group = json!({"chat_id":self.own});
                    for key in [
                        "pending_questions",
                        "pending_bots",
                        "pending_ports",
                        "pending_notices",
                        "pending_folders",
                        "pending_knowledge",
                    ] {
                        group[key] = json!(
                            data.as_array()
                                .into_iter()
                                .flatten()
                                .flat_map(|g| g[key].as_array().into_iter().flatten())
                                .filter(|item| mine(item))
                                .collect::<Vec<_>>()
                        );
                    }
                    data = json!([group]);
                }
                _ => {}
            }
            Ok(data)
        })?
    }
}

pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        if !control::permissions::read().agent_chat_tools {
            return Vec::new();
        }
        vec![
            json!({"name":"control_chat","description":"Drive DMs inside your own task the way the operator would, for example a test DM in an improvement loop. tool is one of the chat tools; arguments are its arguments (chat_create makes a DM spawned below you; others take id). You see your chat and the chats below it, and change only those below it. Turns you start count against the operator's model quota. You cannot decide requests or open folders.","inputSchema":{"type":"object","properties":{
            "tool":{"type":"string","enum":TOOLS},
            "arguments":{"type":"object"}},"required":["tool"],"additionalProperties":false},
            "annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}),
        ]
    }
    fn names(&self) -> &'static [&'static str] {
        &["control_chat"]
    }
    fn call(&self, cx: &AttemptCall<'_>, _name: &str, args: &Value) -> io::Result<Value> {
        if !control::permissions::read().agent_chat_tools {
            return Err(io::Error::other(
                "The operator has not enabled chat tools for agents (Manage → Permissions).",
            ));
        }
        let tool = args["tool"]
            .as_str()
            .filter(|tool| TOOLS.contains(tool))
            .ok_or_else(|| io::Error::other("tool must be one of the chat tools."))?;
        let own = cx.host.0.read(|state| {
            let run = state
                .runtime
                .runs
                .get(cx.attempt.run)
                .filter(|run| {
                    run.status == RunStatus::Running
                        && run.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(cx.attempt.generation)
                })
                .ok_or_else(|| io::Error::other("Attempt is not active"))?;
            crate::identity::authorize(&run.actor, run.action).map_err(io::Error::other)?;
            if crate::delegation::is_judge(run) {
                return Err(io::Error::other("Judges cannot drive chats."));
            }
            run.chat_id
                .clone()
                .ok_or_else(|| io::Error::other("A chat is required"))
        })??;
        let arguments = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let text = call(
            &mut Scoped { shared: cx.host.0, own },
            tool,
            &arguments,
            control::permissions::Approvals::default(),
        )?;
        Ok(json!({"text":text}))
    }
}

#[cfg(test)]
mod tests;
