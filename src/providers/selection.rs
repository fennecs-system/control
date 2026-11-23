//! Selection interprets configuration; it does not call models or change authority.
use super::Model;
use crate::{backend::Backend, runtime::Runtime};

pub(crate) fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "string",
        "maxLength": 128,
        "description": concat!(
            "Optional configured model ID, e.g. claude:opus or codex:MODEL_ID. Omit to inherit the parent's captured ",
            "model. Unknown/unavailable models never silently fall back to another provider. local:NAME selects a ",
            "private endpoint model declared tool-capable; text-only endpoints are refused for agent turns."
        )
    })
}

/// Omission preserves a parent's captured choice even after configuration changes.
pub(crate) fn resolve(
    runtime: &Runtime,
    backend: Backend,
    model: &str,
    selector: Option<&str>,
) -> Result<Model, &'static str> {
    let Some(selector) = selector else {
        return Ok(Model::captured(backend, model.to_owned()));
    };
    let selector = selector.trim();
    let (provider, name) = match selector
        .split_once(':')
        .and_then(|(id, name)| Some((Backend::named(id)?, name)))
    {
        Some((backend, name)) => (Some(backend), name),
        None => (None, selector),
    };
    if !crate::usage::valid_model(name) {
        return Err("Use a configured model ID, optionally prefixed with codex:, claude: or local:.");
    }
    let models = super::choices(runtime);
    let matching: Vec<_> = models
        .iter()
        .filter(|m| m.model == name && provider.is_none_or(|p| m.backend == p))
        .collect();
    let selected = matching
        .iter()
        .find(|m| m.backend == provider.unwrap_or(backend))
        .copied()
        .or_else(|| (matching.len() == 1).then(|| matching[0]))
        .cloned()
        .ok_or("Model is not configured or is ambiguous. Use /model to list choices.")?;
    // Capability, not a listing, decides whether an agent turn can run.
    if !super::agent_capable(runtime, &selected) {
        return Err(crate::backend::LOCAL_TEXT_ONLY);
    }
    Ok(selected)
}

pub(crate) fn command(
    session: &mut crate::chat::Session,
    runtime: &Runtime,
    option: &str,
) -> Result<String, &'static str> {
    crate::identity::authorize(&session.actor, crate::identity::Action::Task)?;
    let option = option.trim();
    let current = session
        .model
        .clone()
        .unwrap_or_else(|| Model::captured(runtime.backend, runtime.configured_model()));
    if option.is_empty() {
        let selected = current;
        // Only selectable models are listed; text-only local models are named once,
        // apart, with the way to enable them.
        let (agents, text): (Vec<_>, Vec<_>) = super::choices(runtime)
            .into_iter()
            .partition(|m| super::agent_capable(runtime, m));
        let names = |models: &[Model]| {
            models
                .iter()
                .map(|m| format!("{}:{}", m.backend.id(), m.model))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let text = if text.is_empty() {
            String::new()
        } else {
            format!(
                " Text-only (control_model_generate and summaries; edit in Manage → Models and enable Serves tool calls to run turns): {}.",
                names(&text)
            )
        };
        return Ok(format!(
            "Selected {}:{}. Configured: {}. Use /model PROVIDER:MODEL or /model default. Changes apply to future turns; active attempts keep their captured model. Listing does not establish account access.{text}",
            selected.backend.id(),
            selected.model,
            names(&agents)
        ));
    }
    let selected = if option == "default" {
        Model::captured(runtime.backend, runtime.configured_model())
    } else {
        resolve(runtime, current.backend, &current.model, Some(option))?
    };
    let reply = format!(
        "Future turns use {}:{}. Active attempts are unchanged.",
        selected.backend.id(),
        selected.model
    );
    session.model = Some(selected);
    Ok(reply)
}

#[cfg(test)]
mod tests;
