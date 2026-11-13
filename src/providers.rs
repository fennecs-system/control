//! Operator-managed model selection; unsupported adapters never silently fall back.
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub(crate) backend: crate::backend::Backend,
    pub(crate) model: String,
    #[serde(default,skip_serializing_if="Option::is_none")]
    capacity:Option<control_context::turn_context::Capacity>,
}
impl Model {
    pub(crate) fn captured(backend:crate::backend::Backend,model:String)->Self {Self {backend,model,capacity:None}}
    fn same(&self,other:&Self)->bool {self.backend==other.backend && self.model==other.model}
}
pub(crate) mod selection;
pub fn capacity(runtime:&crate::runtime::Runtime,backend:crate::backend::Backend,model:&str)->Option<control_context::turn_context::Capacity> {
    runtime.models.iter().find(|m|m.backend==backend && m.model==model).and_then(|m|m.capacity)
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    #[serde(default)]
    models: Vec<Model>,
    #[serde(default)]
    backend: crate::backend::Backend,
    #[serde(default)]
    model: String,
    #[serde(default)]
    text_models: Vec<crate::model_calls::Endpoint>,
    #[serde(default)]
    compaction: crate::compaction::Settings,
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
            ..Default::default()
        }],
        backend,
        model: backend.default_model(),
        ..Default::default()
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
        text_models: state.runtime.compaction.models.clone(),
        compaction: state.runtime.compaction.settings.clone(),
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
            validate_text(&settings.text_models, &settings.compaction)?;
            runtime.compaction.models = settings.text_models;
            runtime.compaction.settings = settings.compaction;
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
                        ..Default::default()
                    }],
                    backend: crate::backend::Backend::Codex,
                    model: model.clone(),
                    ..Default::default()
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
        ..Default::default()
    };
    if settings.models.is_empty() {
        settings.models.push(selected.clone());
        // Older settings retained only the selection. Seed other installed backends.
        for &backend in installed {
            if backend != selected.backend {
                settings.models.push(Model {
                    backend,
                    model: backend.default_model(),
                    ..Default::default()
                });
            }
        }
    }
    validate_models(&settings.models, &selected)?;
    Ok(settings)
}

fn validate_models(models: &[Model], selected: &Model) -> std::io::Result<()> {
    if models.len() > 64
        || !models.iter().any(|m|m.same(selected))
        || models
            .iter()
            .enumerate()
            .any(|(i, m)| !crate::usage::valid_model(&m.model) || models[..i].iter().any(|other|other.same(m)) || m.capacity.is_some_and(|c|c.input_bytes().is_none()))
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
        ..Default::default()
    };
    if !models.iter().any(|m|m.same(&current)) {
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
        let capacity=entry.capacity;
        html.push_str(&format!("<details class='model-edit'><summary>{model}: context capacity</summary><form method='post' action='/providers'><input type='hidden' name='action' value='capacity'><input type='hidden' name='provider' value='{id}'><input type='hidden' name='model' value='{model}'><label>Declared context tokens (empty means unknown)<input name='context_tokens' type='number' min='1' value='{}'></label><label>Output token reserve<input name='output_tokens' type='number' min='1' value='{}'></label><label>Provider/tool overhead reserve<input name='overhead_tokens' type='number' min='0' value='{}'></label><p>Input bytes conservatively count as tokens. Unknown capacity preserves the provider policy. Mandatory instructions that cannot fit are reported, never truncated.</p><button>Save capacity</button></form></details>",capacity.map(|c|c.context_tokens.to_string()).unwrap_or_default(),capacity.map_or(2048,|c|c.output_tokens),capacity.map_or(4096,|c|c.overhead_tokens)));
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
    html.push_str(&render_text(runtime));
    html
}

fn validate_text(models: &[crate::model_calls::Endpoint], settings: &crate::compaction::Settings) -> std::io::Result<()> {
    settings.validate()?;
    if models.len() > 64 || models.iter().enumerate().any(|(at,m)| models[..at].iter().any(|other|other.id==m.id)) {
        return Err(std::io::Error::other("Duplicate or too many text models"));
    }
    for model in models { model.validate()?; }
    if settings.enabled && !models.iter().any(|m|Some(&m.id)==settings.model.as_ref()) {
        return Err(std::io::Error::other("Select a configured compactor model before enabling summaries"));
    }
    Ok(())
}
fn render_text(runtime: &crate::runtime::Runtime) -> String {
    let memory = &runtime.compaction;
    let mut html = "<h3>Text models &amp; compaction</h3><p>External inference services; no model runtime is embedded in Control. These endpoints summarize history for both Codex and Claude; they are not yet tool-using agent adapters.</p>".to_owned();
    for model in &memory.models {
        html.push_str(&format!("<article class='record'><h4>{}</h4><p>{} · context {} tokens · output {} tokens · revision {}</p><form method='post' action='/providers'><input type='hidden' name='action' value='text-remove'><input type='hidden' name='id' value='{}'><button>Remove model</button></form></article>", crate::escape(&model.id), crate::escape(&model.model), model.limits.context_tokens, model.limits.output_tokens, crate::escape(&model.revision), crate::escape(&model.id)));
    }
    html.push_str(concat!("<details><summary>Add compactor model</summary><form method='post' action='/providers'><input type='hidden' name='action' value='text-add'>",
        "<label>Adapter<select name='adapter'><option value='endpoint'>Local endpoint</option><option value='codex'>Codex CLI</option><option value='claude'>Claude CLI</option></select></label>",
        "<label>Name<input name='id' required maxlength='100' placeholder='summary-model'></label><label>Model ID or server alias<input name='model' required maxlength='100'></label>",
        "<label>Private endpoint (leave empty for CLI)<input name='url' placeholder='http://127.0.0.1:8081/v1'></label><label>Weights/runtime revision<input name='revision' required maxlength='256'></label>",
        "<label>Declared context tokens<input name='context_tokens' type='number' min='1024' value='32768'></label><label>Output token reserve<input name='output_tokens' type='number' min='1' value='1024'></label>",
        "<label>Input byte ceiling<input name='input_bytes' type='number' min='1' max='1048576' value='28000'></label><label>Output byte ceiling<input name='output_bytes' type='number' min='1' max='65536' value='8192'></label>",
        "<label>Request timeout seconds<input name='timeout_seconds' type='number' min='1' max='86400' value='1800'></label><button>Add compactor model</button></form></details>"));
    html.push_str("<form method='post' action='/providers'><input type='hidden' name='action' value='compaction'><label>Compaction model<select name='model'><option value=''>Select a text model</option>");
    for model in &memory.models { html.push_str(&format!("<option value='{}'{}>{}</option>",crate::escape(&model.id),if Some(&model.id)==memory.settings.model.as_ref(){" selected"}else{""},crate::escape(&model.id))); }
    html.push_str(&format!("</select></label><label>Chats to summarize (space-separated IDs; empty means none)<input name='chats' maxlength='8192' value='{}'></label>",crate::escape(&memory.settings.chats.join(" "))));
    html.push_str(&format!("<label><input name='enabled' type='checkbox'{}> Automatic summaries</label><details><summary>Advanced window settings</summary><label>Recent entries kept verbatim<input name='recent_entries' type='number' min='1' max='128' value='{}'></label><label>Leaf chunk entries (power of two)<input name='leaf_entries' type='number' min='1' max='64' value='{}'></label><label>History byte budget<input name='history_bytes' type='number' min='1024' max='262144' value='{}'></label></details><p>Input is conservatively byte-bounded using declared context/output limits; this is not exact tokenization. Summary requests exceeding capacity fail visibly. Inspect a chat at /chat/ID/compaction, or use /compact in the TUI.</p><button>Save compaction settings</button></form>", if memory.settings.enabled {" checked"}else{""},memory.settings.recent_entries,memory.settings.leaf_entries,memory.settings.history_bytes));
    html
}
fn save_text(state: &mut crate::State, fields: &HashMap<String,String>, save: impl FnOnce(&crate::State)->std::io::Result<()>) -> Result<(), &'static str> {
    let mut models = state.runtime.compaction.models.clone();
    let mut settings = state.runtime.compaction.settings.clone();
    let get = |name| fields.get(name).map(String::as_str).unwrap_or("");
    match get("action") {
        "text-add" => {
            let context_tokens = get("context_tokens").parse().map_err(|_|"Invalid context size")?;
            let output_tokens = get("output_tokens").parse().map_err(|_|"Invalid output size")?;
            models.push(crate::model_calls::Endpoint { id:get("id").into(),model:get("model").into(),url:get("url").into(),revision:get("revision").into(),
                cli:match get("adapter") {""|"endpoint"=>None,"codex"=>Some(crate::backend::Backend::Codex),"claude"=>Some(crate::backend::Backend::Claude),_=>return Err("Unknown text adapter")},
                limits:control_ai::model::Limits{context_tokens,output_tokens,input_bytes:get("input_bytes").parse().map_err(|_|"Invalid input byte ceiling")?,output_bytes:get("output_bytes").parse().map_err(|_|"Invalid output byte ceiling")?},
                timeout_seconds:get("timeout_seconds").parse().map_err(|_|"Invalid request timeout")? });
        }
        "text-remove" => { models.retain(|m|m.id!=get("id")); }
        "compaction" => {
            settings = crate::compaction::Settings { enabled:fields.contains_key("enabled"), model:(!get("model").is_empty()).then(||get("model").to_owned()),
                chats:get("chats").split_whitespace().map(str::to_owned).collect(),
                recent_entries:get("recent_entries").parse().map_err(|_|"Invalid recent window")?,leaf_entries:get("leaf_entries").parse().map_err(|_|"Invalid leaf size")?,
                history_bytes:get("history_bytes").parse().map_err(|_|"Invalid history budget")? };
        }
        _ => return Err("Unknown model action"),
    }
    validate_text(&models,&settings).map_err(|_|"Invalid model/window settings: check endpoint, limits, unique names and selected model")?;
    let previous_models=std::mem::replace(&mut state.runtime.compaction.models,models);
    let previous_settings=std::mem::replace(&mut state.runtime.compaction.settings,settings);
    if save(state).is_err() {
        state.runtime.compaction.models=previous_models;
        state.runtime.compaction.settings=previous_settings;
        return Err("Could not confirm model settings were saved; reload before retrying");
    }
    state.runtime.compaction.changed();
    crate::worker::wake();
    Ok(())
}
pub fn save_with(
    state: &mut crate::State,
    fields: &HashMap<String, String>,
    save: impl FnOnce(&crate::State) -> std::io::Result<()>,
) -> Result<(), &'static str> {
    if fields.get("action").is_some_and(|a|a.starts_with("text-") || a == "compaction") {
        return save_text(state, fields, save);
    }
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
        ..Default::default()
    };
    let mut models = configured(&state.runtime);
    let mut selected = Model {
        backend: state.runtime.backend,
        model: state.runtime.configured_model(),
        ..Default::default()
    };
    match fields.get("action").map(String::as_str).unwrap_or("use") {
        "capacity"=>{
            let index=models.iter().position(|m|m.same(&entry)).ok_or("Model is not configured")?;
            let context=fields.get("context_tokens").map(|s|s.trim()).unwrap_or("");
            models[index].capacity=if context.is_empty(){None}else{
                let capacity=control_context::turn_context::Capacity{
                    context_tokens:context.parse().map_err(|_|"Invalid context size")?,
                    output_tokens:fields.get("output_tokens").and_then(|s|s.parse().ok()).ok_or("Invalid output reserve")?,
                    overhead_tokens:fields.get("overhead_tokens").and_then(|s|s.parse().ok()).ok_or("Invalid provider/tool reserve")?,
                };
                if capacity.input_bytes().is_none(){return Err("Context capacity must exceed output and provider/tool reserves");}
                Some(capacity)
            };
        }
        "select" => {
            if !models.iter().any(|m|m.same(&entry)) {
                return Err("This model is no longer configured. Reload the list.");
            }
            selected = entry;
        }
        "add" => {
            if models.iter().any(|m|m.same(&entry)) {
                return Err("This model is already configured.");
            }
            models.push(entry);
        }
        "edit" => {
            let old = Model {
                backend,
                model: fields.get("old_model").cloned().unwrap_or_default(),
                ..Default::default()
            };
            let Some(index) = models.iter().position(|m| m.same(&old)) else {
                return Err("This model changed. Reload the list.");
            };
            if !old.same(&entry) && models.iter().any(|m|m.same(&entry)) {
                return Err("This model is already configured.");
            }
            if selected.same(&old) {
                selected = entry.clone();
            }
            models[index] = Model{capacity:if old.same(&entry){models[index].capacity}else{None},..entry};
        }
        "use" => {
            // Compatibility with existing local forms.
            if !models.iter().any(|m|m.same(&entry)) {
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
mod tests;
