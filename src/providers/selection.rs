//! Selection interprets configuration; it does not call models or change authority.
use super::Model;
use crate::{backend::Backend, runtime::Runtime};

pub(crate) fn schema() -> serde_json::Value {
    serde_json::json!({"type":"string","maxLength":128,"description":"Optional configured model ID, e.g. claude:opus or codex:MODEL_ID. Omit to inherit the parent's captured model. Unknown/unavailable models never silently fall back to another provider."})
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
    let (provider, name) = if let Some(name) = selector.strip_prefix("codex:") {
        (Some(Backend::Codex), name)
    } else if let Some(name) = selector.strip_prefix("claude:") {
        (Some(Backend::Claude), name)
    } else {
        (None, selector)
    };
    if !crate::usage::valid_model(name) {
        return Err("Use a configured model ID, optionally prefixed with codex: or claude:.");
    }
    let models = super::configured(runtime);
    let matching: Vec<_> = models
        .iter()
        .filter(|m| m.model == name && provider.is_none_or(|p| m.backend == p))
        .collect();
    let selected = matching
        .iter()
        .find(|m| m.backend == provider.unwrap_or(backend))
        .copied()
        .or_else(|| (matching.len() == 1).then(|| matching[0]));
    selected
        .cloned()
        .ok_or("Model is not configured or is ambiguous. Use /model to list choices.")
}

pub(crate) fn command(
    state: &mut crate::State,
    chat: &str,
    option: &str,
) -> Result<String, &'static str> {
    let index = state
        .chats
        .iter()
        .position(|c| c.id == chat)
        .ok_or("Chat not found.")?;
    crate::identity::authorize(&state.chats[index].actor, crate::identity::Action::Task)?;
    let option = option.trim();
    let current = state.chats[index].model.clone().unwrap_or_else(|| {
        Model::captured(state.runtime.backend, state.runtime.configured_model())
    });
    if option.is_empty() {
        let selected = current;
        let choices = super::configured(&state.runtime)
            .iter()
            .map(|m| format!("{}:{}", m.backend.id(), m.model))
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(format!(
            "Selected {}:{}. Configured: {choices}. Use /model PROVIDER:MODEL or /model default. Changes apply to future turns; active attempts keep their captured model. Listing does not establish account access.",
            selected.backend.id(),
            selected.model
        ));
    }
    let selected = if option == "default" {
        Model::captured(state.runtime.backend, state.runtime.configured_model())
    } else {
        resolve(
            &state.runtime,
            current.backend,
            &current.model,
            Some(option),
        )?
    };
    let reply = format!(
        "Future turns use {}:{}. Active attempts are unchanged.",
        selected.backend.id(),
        selected.model
    );
    state.chats[index].model = Some(selected);
    Ok(reply)
}

#[cfg(test)]
mod tests;
