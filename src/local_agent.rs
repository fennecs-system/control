//! The host's side of a turn on a local endpoint: the endpoint and compactor it
//! runs on, resolved from the registry and the chat. The loop itself is
//! `control_local`'s, behind the same `control_agent` interface as every
//! backend.
use crate::store::Store;
use control_agent::Status;

/// What the host resolved for one local turn.
pub(crate) struct Local {
    pub endpoint: control_agent::Endpoint,
    /// The chat's compactor, which `Host::summarize` and `Host::retain` use.
    pub compactor: crate::model_calls::Endpoint,
    /// What one in-turn summary on it may take and give.
    pub budget: control_agent::Compactor,
}

/// The endpoint and compactor of the attempt's local model; `None` without a
/// live node, which the backend refuses in its own words.
pub(crate) fn resolve(
    attempt: &crate::usage::Attempt,
    bridge: Option<(&Store, &str)>,
) -> Result<Option<Local>, (Status, String)> {
    let Some((shared, run)) = bridge else {
        return Ok(None);
    };
    let (endpoint, compactor) = shared
        .read(|state| {
            let model = crate::providers::Model::captured(crate::backend::Backend::Local, attempt.model.clone());
            if !crate::providers::agent_capable(&state.runtime, &model) {
                return Err((Status::Blocked, crate::backend::LOCAL_TEXT_ONLY.into()));
            }
            let endpoint = crate::providers::endpoint(&state.runtime, &attempt.model)
                .cloned()
                .ok_or((
                    Status::Blocked,
                    "The selected local endpoint is no longer configured; no fallback was used.".to_owned(),
                ))?;
            let session = state
                .runtime
                .runs
                .get(run)
                .and_then(|r| r.chat_id.as_ref())
                .and_then(|chat| state.chats.iter().find(|c| &c.id == chat));
            // A local chat compacts on its own endpoint unless the operator chose otherwise.
            let compactor = session
                .and_then(|s| crate::compaction::compactor_for(state, s))
                .cloned()
                .unwrap_or_else(|| endpoint.clone());
            Ok((endpoint, compactor))
        })
        .map_err(|_| (Status::Failed, "Control state unavailable".to_owned()))??;
    // Registry entries are validated when saved; a model call would refuse
    // one that is not a private endpoint in the same words.
    let url = match endpoint.validate() {
        Ok(crate::model_calls::Target::Endpoint(url)) => url.to_string(),
        Ok(crate::model_calls::Target::Cli(_)) => {
            return Err(model_call_failed("Agent tool loops need a private endpoint"));
        }
        Err(error) => return Err(model_call_failed(&error.to_string())),
    };
    let budget = control_agent::Compactor {
        input_bytes: compactor
            .limits
            .input_budget()
            .saturating_sub(crate::compaction::SYSTEM.len() + crate::compaction::TARGET_BYTES),
        words: crate::compaction::target_words(compactor.limits.output_tokens),
    };
    Ok(Some(Local {
        endpoint: control_agent::Endpoint {
            id: endpoint.id,
            model: endpoint.model,
            revision: endpoint.revision,
            url,
            timeout: std::time::Duration::from_secs(endpoint.timeout_seconds),
            limits: endpoint.limits,
            thinking: endpoint.thinking,
        },
        compactor,
        budget,
    }))
}
fn model_call_failed(error: &str) -> (Status, String) {
    (
        Status::Failed,
        format!(
            "Local model call failed: {error}. No hosted fallback was used; the published checkpoint is unchanged."
        ),
    )
}

#[cfg(test)]
mod tests;
