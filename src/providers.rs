//! Operator-managed model selection; unsupported adapters never silently fall back.
use std::collections::HashMap;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    backend: crate::backend::Backend,
    model: String,
}

pub fn save_settings(state: &crate::State) -> std::io::Result<()> {
    let settings = Settings {
        backend: state.runtime.backend,
        model: state.runtime.configured_model(),
    };
    let bytes = serde_json::to_vec_pretty(&settings).map_err(std::io::Error::other)?;
    crate::config::write_private("model.json", &bytes)
}

pub fn load_settings(runtime: &mut crate::runtime::Runtime) -> std::io::Result<()> {
    let path = crate::config::directory()
        .map_err(std::io::Error::other)?
        .join("model.json");
    match std::fs::read(&path) {
        Ok(bytes) => {
            let settings: Settings = serde_json::from_slice(&bytes)
                .map_err(|_| std::io::Error::other("Invalid model.json configuration"))?;
            if !crate::usage::valid_model(&settings.model) {
                return Err(std::io::Error::other("Invalid model ID in model.json"));
            }
            runtime.model = Some(settings.model);
            runtime.backend = settings.backend;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(model) = &runtime.model {
                if !crate::usage::valid_model(model) {
                    return Err(std::io::Error::other("Invalid legacy model configuration"));
                }
                // Preserve an old default before the next checkpoint omits it.
                let bytes = serde_json::to_vec_pretty(&Settings {
                    backend: crate::backend::Backend::Codex,
                    model: model.clone(),
                })
                .map_err(std::io::Error::other)?;
                crate::config::write_private("model.json", &bytes)?;
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub fn render(runtime: &crate::runtime::Runtime) -> String {
    let model = crate::escape(&runtime.configured_model());
    let backend = runtime.backend;
    let other = if backend == crate::backend::Backend::Codex {
        crate::backend::Backend::Claude
    } else {
        crate::backend::Backend::Codex
    };
    let label = backend.label();
    let id = backend.id();
    let other_label = other.label();
    let other_id = other.id();
    let other_model = crate::escape(&other.default_model());
    format!("<div class='section-heading'><h2>Models &amp; providers</h2><details class='provider-new'><summary class='primary'>+ New</summary><form method='post' action='/providers'><input type='hidden' name='provider' value='{other_id}'><input type='hidden' name='model' value='{other_model}'><button>{other_label}</button></form></details></div><p id='saved' class='provider-saved save-confirmation' role='status'>Default model updated.</p><section class='record provider-current'><h3>{label} <span class='tag'>Active</span></h3><p class='muted'>Default · <code>{model}</code></p><details class='provider-edit'><summary>Edit</summary><form method='post' action='/providers'><input type='hidden' name='provider' value='{id}'><label>Default model<input name='model' required maxlength='100' value='{model}' spellcheck='false' autocapitalize='none'></label><p class='muted'>Applies to new runs.</p><button class='primary'>Update default</button></form></details></section>")
}
pub fn save_with(
    state: &mut crate::State,
    fields: &HashMap<String, String>,
    save: impl FnOnce(&crate::State) -> std::io::Result<()>,
) -> Result<(), &'static str> {
    let backend = match fields.get("provider").map(String::as_str) {
        Some("codex") => crate::backend::Backend::Codex,
        Some("claude") => crate::backend::Backend::Claude,
        _ => return Err("This provider adapter is not connected. The model was not changed."),
    };
    let model = fields.get("model").map(|s| s.trim()).unwrap_or("");
    if !crate::usage::valid_model(model) {
        return Err("Enter a model ID of 1–100 letters, digits, dots, hyphens, underscores, colons or slashes.");
    }
    let previous = state.runtime.model.replace(model.to_owned());
    let previous_backend = std::mem::replace(&mut state.runtime.backend, backend);
    if save(state).is_err() {
        state.runtime.model = previous;
        state.runtime.backend = previous_backend;
        return Err(
            "Could not confirm the model setting was saved. Reload settings before retrying.",
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backend_selection_is_captured_and_failed_settings_do_not_reroute_runs() {
        use crate::backend::Backend;
        let mut state = crate::State::default();
        state.runtime.enqueue("Codex task".into(), "task", "test");
        let fields = HashMap::from([
            ("provider".into(), "claude".into()),
            ("model".into(), "sonnet".into()),
        ]);
        assert!(
            save_with(&mut state, &fields, |_| Err(std::io::Error::other(
                "disk full"
            )))
            .is_err()
        );
        assert_eq!(state.runtime.backend, Backend::Codex);
        save_with(&mut state, &fields, |_| Ok(())).unwrap();
        state.runtime.enqueue("Claude task".into(), "task", "test");
        assert_eq!(state.runtime.runs[0].backend, Backend::Codex);
        assert_eq!(state.runtime.runs[1].backend, Backend::Claude);
        let mut restored: crate::State =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(restored.runtime.runs[1].backend, Backend::Claude);
        let id = restored.runtime.runs[1].id.clone();
        restored.runtime.runs[1].status = "failed".into();
        restored
            .runtime
            .action("/retry-run", &HashMap::from([("id".into(), id)]));
        assert_eq!(restored.runtime.runs[1].backend, Backend::Claude);
        assert_eq!(restored.runtime.runs[1].model.as_deref(), Some("sonnet"));
    }
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
        let checkpoint = serde_json::to_value(&state).unwrap();
        assert!(checkpoint["runtime"].get("model").is_none());
        assert_eq!(checkpoint["runtime"]["runs"][0]["model"], "test-model-one");
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
