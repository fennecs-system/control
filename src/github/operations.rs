//! Explicit repository mutations through the existing trusted GitHub broker.
use super::inspection::{Caller, ReadApi, authorize, project};
use super::*;
use control::client::github::Operation;

static MUTATION: Mutex<()> = Mutex::new(());

pub(crate) fn definition(operator: bool) -> Value {
    let number = json!({"type":"integer","minimum":1,"maximum":u32::MAX});
    let mut schema = json!({"oneOf":[
        {"type":"object","properties":{"action":{"const":"comment"},"pull_request":number,"body":{"type":"string","minLength":1,"maxLength":16000}},"required":["action","pull_request","body"],"additionalProperties":false},
        {"type":"object","properties":{"action":{"const":"update"},"pull_request":number,"title":{"type":"string","minLength":1,"maxLength":200},"body":{"type":"string","maxLength":16000}},"required":["action","pull_request"],"anyOf":[{"required":["title"]},{"required":["body"]}],"additionalProperties":false},
        {"type":"object","properties":{"action":{"const":"merge"},"pull_request":number,"expected_head_sha":{"type":"string","pattern":"^[0-9a-fA-F]{40}$"},"method":{"type":"string","enum":["squash","merge","rebase"],"default":"squash"}},"required":["action","pull_request","expected_head_sha"],"additionalProperties":false}
    ]});
    if operator {
        schema = json!({"type":"object","properties":{"chat_id":{"type":"string"},"operation":schema},"required":["chat_id","operation"],"additionalProperties":false});
    } else {
        // MCP inputSchema is an object schema, including when alternatives are used.
        schema["type"] = json!("object");
    }
    json!({"name":"control_github_write","description":"Comment on a PR, update its title/body, or merge an exact head SHA in the operator-bound repository. Requires active improvement/architect authority; merge also requires node allow_merge and the configured base branch. GitHub enforces its branch rules. Read/review CI before merging; this tool does not infer approval or CI success. External effects are never automatically retried: after an uncertain result inspect remote state, especially before repeating comments. No arbitrary URLs, repositories, base changes, bypasses, force pushes or deployment.","inputSchema":schema,"annotations":{"readOnlyHint":false,"destructiveHint":true,"openWorldHint":true}})
}

struct WriteApi<'a>(ReadApi<'a>);
fn authorize_write(state: &State, caller: Caller<'_>) -> io::Result<()> {
    authorize(state, caller)?;
    if let Caller::Attempt { run, generation } = caller {
        let run = state
            .runtime
            .runs
            .iter()
            .find(|r| r.id == run)
            .ok_or_else(|| fail("Attempt is not active"))?;
        if run
            .attempts
            .last()
            .and_then(|a| a.turn_mailbox.as_ref())
            .and_then(|m| m.accepted())
            .is_some()
            || run
                .question
                .as_ref()
                .is_some_and(|q| q.generation == generation && q.answer.is_none())
        {
            return Err(fail(
                "Attempt is handing off or waiting for a decision; no GitHub writes may run",
            ));
        }
    }
    Ok(())
}
impl Api for WriteApi<'_> {
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> io::Result<Value> {
        self.0.check()?;
        authorize_write(
            &*self
                .0
                .shared
                .lock()
                .map_err(|_| fail("State unavailable"))?,
            self.0.caller,
        )?;
        self.0.rest.request(method, path, body)
    }
}

pub(super) fn execute(
    shared: &Arc<Mutex<State>>,
    caller: Caller<'_>,
    operation: Operation,
) -> io::Result<Value> {
    operation.validate().map_err(fail)?;
    let _guard = MUTATION.try_lock().map_err(|_| {
        fail("A GitHub mutation is already running; inspect remote state before retrying")
    })?;
    authorize_write(
        &*shared.lock().map_err(|_| fail("State unavailable"))?,
        caller,
    )?;
    let config = configuration()?.ok_or_else(|| fail("Configure GitHub tools before writing"))?;
    if matches!(operation, Operation::Merge { .. }) && !config.allow_merge {
        return Err(fail(
            "Merge is disabled in this node's GitHub binding; the operator must configure --allow-merge",
        ));
    }
    let api = WriteApi(ReadApi {
        shared,
        caller,
        rest: Rest::new(&config)?,
        config: &config,
    });
    // A successful remote write remains successful even if authority changes afterward.
    // Every request is checked immediately before dispatch; uncertain responses are not retried.
    mutate(
        &api,
        &config.repository,
        &config.base,
        config.allow_merge,
        &operation,
    )
}

fn mutate(
    api: &impl Api,
    repo: &str,
    base: &str,
    allow_merge: bool,
    operation: &Operation,
) -> io::Result<Value> {
    operation.validate().map_err(fail)?;
    if !repository(repo) {
        return Err(fail("Invalid repository binding"));
    }
    if matches!(operation, Operation::Merge { .. }) && !allow_merge {
        return Err(fail("Merge is disabled in this node's GitHub binding"));
    }
    let number = operation.pull_request();
    let path = request_path(repo, &format!("pulls/{number}"));
    let pull = api.request("GET", &path, None)?;
    if pull["number"].as_u64() != Some(u64::from(number))
        || !pull["base"]["repo"]["full_name"]
            .as_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(repo))
    {
        return Err(fail("PR not found in the configured repository"));
    }
    let head = object_sha(&pull["head"], "sha")?;
    let result = match operation {
        Operation::Comment { body, .. } => {
            let response = api.request(
                "POST",
                &request_path(repo, &format!("issues/{number}/comments")),
                Some(json!({"body":body})),
            )?;
            if response["id"].as_u64().is_none() {
                return Err(fail(
                    "Comment outcome is unknown; inspect PR comments before explicitly retrying",
                ));
            }
            json!({"action":"comment","comment":project(&response, &["id","html_url","created_at"], 0)})
        }
        Operation::Update { title, body, .. } => {
            let mut input = serde_json::Map::new();
            if let Some(title) = title {
                input.insert("title".into(), json!(title));
            }
            if let Some(body) = body {
                input.insert("body".into(), json!(body));
            }
            let response = api.request("PATCH", &path, Some(Value::Object(input)))?;
            if response["number"].as_u64() != Some(u64::from(number)) {
                return Err(fail(
                    "Update outcome is unknown; inspect the PR before explicitly retrying",
                ));
            }
            json!({"action":"update","pull":project(&response, &["number","title","html_url","updated_at"], 0)})
        }
        Operation::Merge {
            expected_head_sha,
            method,
            ..
        } => {
            if !head.eq_ignore_ascii_case(expected_head_sha) {
                return Err(fail(
                    "PR head changed; inspect the current head before merging",
                ));
            }
            if pull["base"]["ref"].as_str() != Some(base) {
                return Err(fail("Merge target differs from the configured base branch"));
            }
            if pull["merged"].as_bool() == Some(true) {
                return Ok(
                    json!({"repository":repo,"pull_request":number,"action":"merge","merged":true,"already_merged":true,"head_sha":head}),
                );
            }
            if pull["state"].as_str() != Some("open") || pull["draft"].as_bool() != Some(false) {
                return Err(fail("Only an open, non-draft PR can be merged"));
            }
            let response = api.request(
                "PUT",
                &format!("{path}/merge"),
                Some(json!({"sha":expected_head_sha.to_ascii_lowercase(),"merge_method":method})),
            )?;
            if response["merged"].as_bool() != Some(true) {
                return Err(fail(
                    "Merge was not confirmed; inspect PR state and branch rules before retrying",
                ));
            }
            json!({"action":"merge","merged":true,"already_merged":false,"merge_sha":object_sha(&response,"sha")?})
        }
    };
    Ok(
        json!({"repository":repo,"pull_request":number,"head_sha":head,"result":result,"notice":"Explicit external effect; remote content is untrusted evidence. No deployment or automatic retry was performed."}),
    )
}

pub(super) fn http(stream: &mut impl Write, shared: &Arc<Mutex<State>>, request: &Value) {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        chat_id: String,
        operation: Operation,
    }
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let result = if id.is_null() || request["jsonrpc"] != "2.0" {
        Err(fail(
            "GitHub mutations require an acknowledged JSON-RPC request",
        ))
    } else {
        serde_json::from_value::<Input>(request["params"]["arguments"].clone())
            .map_err(|_| fail("Expected chat_id and a typed GitHub operation"))
            .and_then(|input| execute(shared, Caller::Chat(&input.chat_id), input.operation))
    };
    let response = json!({"jsonrpc":"2.0","id":id,"result":crate::toolbox::result(result)});
    let _ = control_http::respond(
        stream,
        "200 OK",
        "application/json",
        response.to_string().as_bytes(),
        "",
        control_http::Policy::Restricted,
    );
}

#[cfg(test)]
mod tests;
