//! Operator-managed model selection; unsupported adapters never silently fall back.
use std::collections::HashMap;

pub fn render(runtime: &crate::runtime::Runtime) -> String {
    let model = crate::escape(&runtime.configured_model());
    format!("<div class='section-heading'><h2>Models &amp; providers</h2><details class='provider-new'><summary class='primary'>+ New</summary><div class='provider-options'><p class='muted'>Additional adapters are not available yet.</p><button disabled>OpenAI API</button><button disabled>Anthropic</button><button disabled>Self-hosted</button></div></details></div><p id='saved' class='provider-saved save-confirmation' role='status'>Default model updated.</p><section class='record provider-current'><h3>OpenAI · Codex <span class='tag'>Active</span></h3><p class='muted'>Default · <code>{model}</code></p><details class='provider-edit'><summary>Edit</summary><form method='post' action='/providers'><input type='hidden' name='provider' value='codex'><label>Default model<input name='model' required maxlength='100' value='{model}' spellcheck='false' autocapitalize='none'></label><p class='muted'>Applies to new runs.</p><button class='primary'>Update default</button></form></details></section>")
}
pub fn save_with(
    state: &mut crate::State,
    fields: &HashMap<String, String>,
    save: impl FnOnce(&crate::State) -> std::io::Result<()>,
) -> Result<(), &'static str> {
    if fields.get("provider").map(String::as_str) != Some("codex") {
        return Err("This provider adapter is not connected. The model was not changed.");
    }
    let model = fields.get("model").map(|s| s.trim()).unwrap_or("");
    if !crate::usage::valid_model(model) {
        return Err("Enter a model ID of 1–100 letters, digits, dots, hyphens, underscores, colons or slashes.");
    }
    let previous = state.runtime.model.replace(model.to_owned());
    if save(state).is_err() {
        state.runtime.model = previous;
        return Err("Could not save the model. The previous setting is unchanged.");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_settings_persist_atomically_and_runs_keep_their_selected_model() {
        let mut state = crate::State::default();
        let mut fields = HashMap::from([
            ("provider".into(), "codex".into()),
            ("model".into(), "test-model-one".into()),
        ]);
        save_with(&mut state, &fields, |_| Ok(())).unwrap();
        state.runtime.enqueue("First".into(), "task", "test");
        fields.insert("model".into(), "test-model-two".into());
        assert!(
            save_with(&mut state, &fields, |_| Err(std::io::Error::other(
                "disk full"
            )))
            .is_err()
        );
        assert_eq!(state.runtime.configured_model(), "test-model-one");
        save_with(&mut state, &fields, |_| Ok(())).unwrap();
        state.runtime.enqueue("Second".into(), "task", "test");
        assert_eq!(
            state.runtime.runs[0].model.as_deref(),
            Some("test-model-one")
        );
        assert_eq!(
            state.runtime.runs[1].model.as_deref(),
            Some("test-model-two")
        );
        let restored: crate::State =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(restored.runtime.configured_model(), "test-model-two");
        fields.insert("provider".into(), "anthropic".into());
        assert!(save_with(&mut state, &fields, |_| panic!(
            "must not persist unsupported adapter"
        ))
        .is_err());
        fields.insert("provider".into(), "codex".into());
        fields.insert("model".into(), "--override\nsecret".into());
        assert!(save_with(&mut state, &fields, |_| panic!(
            "must not persist invalid ID"
        ))
        .is_err());
    }
}
