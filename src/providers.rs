//! Operator-managed model selection; unsupported adapters never silently fall back.
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    backend: crate::backend::Backend,
    model: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    models: Vec<Model>,
    #[serde(default)]
    backend: crate::backend::Backend,
    #[serde(default)]
    model: String,
}

pub fn initialization_options(
    args: &[String],
) -> std::io::Result<(Option<crate::backend::Backend>, Option<std::path::PathBuf>)> {
    let mut provider = None;
    let mut source = None;
    let (options, remainder) = args.as_chunks::<2>();
    for pair in options {
        match pair[0].as_str() {
            "--provider" if provider.is_none() => {
                provider = Some(match pair[1].as_str() {
                    "codex" => crate::backend::Backend::Codex,
                    "claude" => crate::backend::Backend::Claude,
                    _ => return Err(std::io::Error::other("Provider must be codex or claude")),
                });
            }
            "--source-root" if source.is_none() && std::path::Path::new(&pair[1]).is_absolute() => {
                source = Some(std::path::PathBuf::from(&pair[1]))
            }
            _ => {
                return Err(std::io::Error::other(
                    "Use config init [--provider codex|claude] [--source-root /absolute/checkout]",
                ))
            }
        }
    }
    if !remainder.is_empty() {
        return Err(std::io::Error::other(
            "Configuration option requires a value",
        ));
    }
    Ok((provider, source))
}

/// Setup chooses the first provider explicitly, without replacing a saved library.
pub fn initialize(backend: crate::backend::Backend) -> std::io::Result<()> {
    initialize_in(
        &crate::config::directory().map_err(std::io::Error::other)?,
        backend,
    )
}

fn initialize_in(
    directory: &std::path::Path,
    backend: crate::backend::Backend,
) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = directory.join("model.json");
    let settings = Settings {
        models: vec![Model {
            backend,
            model: backend.default_model(),
        }],
        backend,
        model: backend.default_model(),
    };
    if path.try_exists()? || std::fs::symlink_metadata(&path).is_ok() {
        return Ok(());
    }
    let temporary = directory.join(format!(".model-init.{}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(&serde_json::to_vec_pretty(&settings).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        // Publish only complete settings, without replacing a racing setup or operator save.
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => std::fs::File::open(directory)?.sync_all(),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    })();
    let cleanup = std::fs::remove_file(temporary);
    result.and(cleanup)
}

pub fn save_settings(state: &crate::State) -> std::io::Result<()> {
    let settings = Settings {
        models: configured(&state.runtime),
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
            let installed: Vec<_> = [
                crate::backend::Backend::Codex,
                crate::backend::Backend::Claude,
            ]
            .into_iter()
            .filter(|b| b.image().is_some())
            .collect();
            let settings = migrate(settings, &installed)?;
            runtime.models = settings.models;
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
                    models: vec![Model {
                        backend: crate::backend::Backend::Codex,
                        model: model.clone(),
                    }],
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

fn migrate(
    mut settings: Settings,
    installed: &[crate::backend::Backend],
) -> std::io::Result<Settings> {
    // A library with one entry needs no separate selection. Multiple entries do:
    // never guess over an explicit choice or silently reroute an invalid selection.
    if settings.model.is_empty() && settings.models.len() == 1 {
        settings.backend = settings.models[0].backend;
        settings.model = settings.models[0].model.clone();
    }
    let selected = Model {
        backend: settings.backend,
        model: settings.model.clone(),
    };
    if settings.models.is_empty() {
        settings.models.push(selected.clone());
        // Older settings retained only the selection. Seed other installed backends.
        for &backend in installed {
            if backend != selected.backend {
                settings.models.push(Model {
                    backend,
                    model: backend.default_model(),
                });
            }
        }
    }
    validate_models(&settings.models, &selected)?;
    Ok(settings)
}

fn validate_models(models: &[Model], selected: &Model) -> std::io::Result<()> {
    if models.len() > 64
        || !models.contains(selected)
        || models
            .iter()
            .enumerate()
            .any(|(i, m)| !crate::usage::valid_model(&m.model) || models[..i].contains(m))
    {
        return Err(std::io::Error::other(
            "Invalid saved models or default in model.json",
        ));
    }
    Ok(())
}
fn configured(runtime: &crate::runtime::Runtime) -> Vec<Model> {
    let mut models = runtime.models.clone();
    let current = Model {
        backend: runtime.backend,
        model: runtime.configured_model(),
    };
    if !models.contains(&current) {
        models.push(current);
    }
    models
}

pub fn render(runtime: &crate::runtime::Runtime) -> String {
    let mut html = String::from("<h2>Models &amp; providers</h2><p id='saved' class='provider-saved save-confirmation' role='status'>Saved.</p><div class='model-list' aria-label='Configured models'>");
    for entry in configured(runtime) {
        let model = crate::escape(&entry.model);
        let id = entry.backend.id();
        let label = entry.backend.label();
        let selected =
            entry.backend == runtime.backend && entry.model == runtime.configured_model();
        let marker = if selected {
            "<span class='model-default'>Default</span>"
        } else {
            ""
        };
        html.push_str(&format!("<section class='model-row'><form method='post' action='/providers'><input type='hidden' name='action' value='select'><input type='hidden' name='provider' value='{id}'><input type='hidden' name='model' value='{model}'><button class='model-choice' aria-pressed='{selected}'><span class='model-dot' aria-hidden='true'></span><span class='model-name'>{model}<span class='model-provider'>{label}</span></span>{marker}</button></form><details class='model-edit'><summary>Edit</summary><form method='post' action='/providers'><input type='hidden' name='action' value='edit'><input type='hidden' name='provider' value='{id}'><input type='hidden' name='old_model' value='{model}'><label>Model<input name='model' required maxlength='100' value='{model}' spellcheck='false' autocapitalize='none'></label><button>Save</button></form></details></section>"));
    }
    html.push_str("</div><details class='model-add'><summary>+ New model</summary>");
    for backend in [
        crate::backend::Backend::Codex,
        crate::backend::Backend::Claude,
    ] {
        let id = backend.id();
        let label = backend.label();
        let default = crate::escape(&backend.default_model());
        html.push_str(&format!("<form method='post' action='/providers'><input type='hidden' name='action' value='add'><input type='hidden' name='provider' value='{id}'><label>{label}<input name='model' required maxlength='100' value='{default}' spellcheck='false' autocapitalize='none'></label><button>Add model</button></form>"));
    }
    html.push_str("</details>");
    html
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
    let entry = Model {
        backend,
        model: model.to_owned(),
    };
    let mut models = configured(&state.runtime);
    let mut selected = Model {
        backend: state.runtime.backend,
        model: state.runtime.configured_model(),
    };
    match fields.get("action").map(String::as_str).unwrap_or("use") {
        "select" => {
            if !models.contains(&entry) {
                return Err("This model is no longer configured. Reload the list.");
            }
            selected = entry;
        }
        "add" => {
            if models.contains(&entry) {
                return Err("This model is already configured.");
            }
            models.push(entry);
        }
        "edit" => {
            let old = Model {
                backend,
                model: fields.get("old_model").cloned().unwrap_or_default(),
            };
            let Some(index) = models.iter().position(|m| m == &old) else {
                return Err("This model changed. Reload the list.");
            };
            if old != entry && models.contains(&entry) {
                return Err("This model is already configured.");
            }
            if selected == old {
                selected = entry.clone();
            }
            models[index] = entry;
        }
        "use" => {
            // Compatibility with existing local forms.
            if !models.contains(&entry) {
                models.push(entry.clone());
            }
            selected = entry;
        }
        _ => return Err("Unknown model action."),
    }
    if models.len() > 64 {
        return Err("At most 64 models can be configured.");
    }
    let previous = state.runtime.model.replace(selected.model);
    let previous_backend = std::mem::replace(&mut state.runtime.backend, selected.backend);
    let previous_models = std::mem::replace(&mut state.runtime.models, models);
    if save(state).is_err() {
        state.runtime.model = previous;
        state.runtime.backend = previous_backend;
        state.runtime.models = previous_models;
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
    fn fresh_claude_setup_selects_sonnet_and_reruns_preserve_the_library() {
        use crate::backend::Backend;
        use std::os::unix::fs::PermissionsExt;
        let directory =
            std::env::temp_dir().join(format!("control-model-init-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        initialize_in(&directory, Backend::Claude).unwrap();
        let path = directory.join("model.json");
        let original = std::fs::read(&path).unwrap();
        let settings = migrate(
            serde_json::from_slice(&original).unwrap(),
            &[Backend::Codex, Backend::Claude],
        )
        .unwrap();
        assert_eq!(settings.backend, Backend::Claude);
        assert_eq!(settings.model, "sonnet");
        assert_eq!(
            settings.models,
            vec![Model {
                backend: Backend::Claude,
                model: "sonnet".into()
            }]
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        initialize_in(&directory, Backend::Codex).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        // An existing explicit custom/default library is equally immutable to setup.
        let custom = br#"{"models":[{"backend":"codex","model":"custom"}],"backend":"codex","model":"custom"}"#;
        std::fs::write(&path, custom).unwrap();
        initialize_in(&directory, Backend::Claude).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), custom);
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn sole_configured_model_is_selected_without_an_explicit_default() {
        use crate::backend::Backend;
        let settings = migrate(
            serde_json::from_str(r#"{"models":[{"backend":"claude","model":"sonnet"}]}"#).unwrap(),
            &[Backend::Codex, Backend::Claude],
        )
        .unwrap();
        assert_eq!(settings.backend, Backend::Claude);
        assert_eq!(settings.model, "sonnet");
        assert_eq!(settings.models.len(), 1);
        let ambiguous = r#"{"models":[{"backend":"claude","model":"sonnet"},{"backend":"claude","model":"opus"}]}"#;
        assert!(migrate(serde_json::from_str(ambiguous).unwrap(), &[]).is_err());
        let explicit_wrong = r#"{"models":[{"backend":"claude","model":"sonnet"}],"backend":"codex","model":"custom"}"#;
        assert!(migrate(serde_json::from_str(explicit_wrong).unwrap(), &[]).is_err());
    }
    #[test]
    fn provider_forms_have_bound_prefilled_defaults() {
        let html = render(&crate::runtime::Runtime::default());
        let forms = html.split("<details class='model-add'>").nth(1).unwrap();
        let claude = forms
            .split("name='provider' value='claude'")
            .nth(1)
            .unwrap();
        assert!(claude.contains("value='sonnet'"));
        assert!(forms.contains(&format!(
            "value='{}'",
            crate::backend::Backend::Codex.default_model()
        )));
        assert!(!forms.contains("type='radio'"));
    }
    #[test]
    fn init_options_are_explicit_and_reject_invalid_or_repeated_flags() {
        let parse = |args: &[&str]| {
            initialization_options(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(parse(&[]).unwrap(), (None, None));
        assert_eq!(
            parse(&["--provider", "claude", "--source-root", "/checkout"]).unwrap(),
            (
                Some(crate::backend::Backend::Claude),
                Some("/checkout".into())
            )
        );
        for args in [
            vec!["--provider"],
            vec!["--provider", "other"],
            vec!["--source-root", "relative"],
            vec!["--provider", "claude", "--provider", "codex"],
            vec!["--unknown", "value"],
        ] {
            assert!(parse(&args).is_err());
        }
    }
    #[test]
    fn legacy_selection_migrates_and_saved_libraries_roundtrip_without_reseeding() {
        use crate::backend::Backend;
        let old: Settings = serde_json::from_str(r#"{"model":"custom-codex"}"#).unwrap();
        let mut settings = migrate(old, &[Backend::Codex, Backend::Claude]).unwrap();
        assert_eq!(settings.backend, Backend::Codex);
        assert_eq!(settings.model, "custom-codex");
        assert_eq!(settings.models.len(), 2);
        settings.models[1].model = "custom-claude".into();
        let reloaded = migrate(
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap(),
            &[Backend::Codex, Backend::Claude],
        )
        .unwrap();
        assert_eq!(reloaded.models, settings.models);
        let invalid = Settings {
            backend: Backend::Claude,
            model: "not-in-list".into(),
            models: settings.models,
        };
        assert!(migrate(invalid, &[]).is_err());
    }
    #[test]
    fn adding_selecting_and_editing_keep_the_library_and_selection_separate() {
        use crate::backend::Backend;
        let mut state = crate::State::default();
        let original = state.runtime.configured_model();
        let mut fields = HashMap::from([
            ("action".into(), "add".into()),
            ("provider".into(), "claude".into()),
            ("model".into(), "sonnet".into()),
        ]);
        save_with(&mut state, &fields, |_| Ok(())).unwrap();
        assert_eq!(state.runtime.backend, Backend::Codex);
        assert_eq!(state.runtime.configured_model(), original);
        assert_eq!(state.runtime.models.len(), 2);
        fields.insert("action".into(), "select".into());
        save_with(&mut state, &fields, |_| Ok(())).unwrap();
        state
            .runtime
            .enqueue("Uses saved selection".into(), "task", "test");
        fields.insert("action".into(), "edit".into());
        fields.insert("old_model".into(), "sonnet".into());
        fields.insert("model".into(), "opus".into());
        let previous_models = state.runtime.models.clone();
        assert!(
            save_with(&mut state, &fields, |_| Err(std::io::Error::other(
                "disk full"
            )))
            .is_err()
        );
        assert_eq!(state.runtime.models, previous_models);
        assert_eq!(state.runtime.configured_model(), "sonnet");
        save_with(&mut state, &fields, |_| Ok(())).unwrap();
        assert_eq!(state.runtime.configured_model(), "opus");
        assert_eq!(state.runtime.models.len(), 2);
        assert_eq!(state.runtime.runs[0].model.as_deref(), Some("sonnet"));
        fields.insert("action".into(), "select".into());
        fields.insert("model".into(), "unknown-model".into());
        assert!(save_with(&mut state, &fields, |_| panic!(
            "unknown selection must not save"
        ))
        .is_err());
        assert_eq!(state.runtime.configured_model(), "opus");
    }
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
