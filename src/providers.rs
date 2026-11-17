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
    if backend==crate::backend::Backend::Local {
        // Declared endpoint limits, reserving the tool loop's schemas and repair budget.
        return endpoint(runtime,model).map(|e|control_context::turn_context::Capacity{
            context_tokens:e.limits.context_tokens,output_tokens:e.limits.output_tokens,
            overhead_tokens:crate::local_agent::OVERHEAD_TOKENS});
    }
    runtime.models.iter().find(|m|m.backend==backend && m.model==model).and_then(|m|m.capacity)
}
/// The private endpoint a `local:NAME` selection refers to.
pub(crate) fn endpoint<'a>(runtime:&'a crate::runtime::Runtime,name:&str)->Option<&'a crate::model_calls::Endpoint> {
    local(runtime).find(|m|m.id==name)
}
/// Capability, not a listing, decides whether an agent turn can run.
pub(crate) fn agent_capable(runtime:&crate::runtime::Runtime,model:&Model)->bool {
    model.backend!=crate::backend::Backend::Local || endpoint(runtime,&model.model).is_some_and(|e|e.tools)
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

/// Returns local model names retired by merging duplicate entries, with their
/// replacements, so a caller holding chats can repoint their selections.
pub fn load_settings(runtime: &mut crate::runtime::Runtime) -> std::io::Result<Vec<(String, String)>> {
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
            let mut settings = migrate(settings, &installed)?;
            let renamed = merge_duplicates(&mut settings.text_models, &mut settings.compaction);
            validate_text(&settings.text_models, &settings.compaction)?;
            runtime.compaction.models = settings.text_models;
            runtime.compaction.settings = settings.compaction;
            runtime.models = settings.models;
            runtime.model = Some(settings.model);
            runtime.backend = settings.backend;
            Ok(renamed)
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
            Ok(Vec::new())
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
        || !selected.backend.default_capable()
        || models.iter().any(|m|!m.backend.default_capable())
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
/// Everything a chat or spawn may name: saved agent models plus each private
/// endpoint text model, listed with its capability rather than as interchangeable.
pub(crate) fn choices(runtime: &crate::runtime::Runtime) -> Vec<Model> {
    let mut models = configured(runtime);
    models.extend(local(runtime).map(|m|Model::captured(crate::backend::Backend::Local,m.id.clone())));
    models
}
fn local(runtime: &crate::runtime::Runtime) -> impl Iterator<Item=&crate::model_calls::Endpoint> {
    runtime.compaction.models.iter().filter(|m|m.cli.is_none())
}

pub fn render(runtime: &crate::runtime::Runtime) -> String {
    let mut html = String::from("<div class='providers-page'><h2>Models &amp; providers</h2><p class='provider-description'>Choose the default agent model and configure optional history summaries.</p><p id='saved' class='provider-saved save-confirmation' role='status'>Saved.</p><section class='provider-section' aria-labelledby='agent-models-heading'><h3 id='agent-models-heading'>Agent models</h3><p class='provider-description'>Used for DMs and newly spawned agents. Select a model to make it the default.</p><div class='model-list' aria-label='Configured models'>");
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
        html.push_str(&format!("<section class='model-row'><form method='post' action='/providers'><input type='hidden' name='action' value='select'><input type='hidden' name='provider' value='{id}'><input type='hidden' name='model' value='{model}'><button class='model-choice' aria-pressed='{selected}'><span class='model-dot' aria-hidden='true'></span><span class='model-name'><span class='model-id' title='{model}'>{model}</span><span class='model-provider'>{label}</span></span>{marker}</button></form><details class='model-edit'><summary aria-label='Edit {label} model {model}'>Edit</summary><div class='model-edit-body'><form method='post' action='/providers'><input type='hidden' name='action' value='edit'><input type='hidden' name='provider' value='{id}'><input type='hidden' name='old_model' value='{model}'><label>Model ID<input name='model' required maxlength='100' value='{model}' spellcheck='false' autocapitalize='none'></label><button class='primary'>Save model</button></form>"));
        html.push_str(&format!("<form class='model-capacity' method='post' action='/providers'><input type='hidden' name='action' value='capacity'><input type='hidden' name='provider' value='{id}'><input type='hidden' name='model' value='{model}'><h4>Context capacity</h4><p class='provider-description'>Leave context tokens empty to use the provider policy. Instructions that cannot fit are reported, never truncated.</p><div class='provider-fields'><label>Context tokens<input name='context_tokens' type='number' min='1' placeholder='Unknown' value='{}'></label><label>Output token reserve<input name='output_tokens' type='number' min='1' value='{}'></label><label>Provider/tool token reserve<input name='overhead_tokens' type='number' min='0' value='{}'></label></div><p class='provider-description'>Input bytes conservatively count as tokens.</p><button class='primary'>Save capacity</button></form></div></details></section>",capacity.map(|c|c.context_tokens.to_string()).unwrap_or_default(),capacity.map_or(2048,|c|c.output_tokens),capacity.map_or(4096,|c|c.overhead_tokens)));
    }
    // Local endpoints are listed once, under Text models, with their capabilities;
    // they are chosen per chat or spawn and never become the node default.
    html.push_str("</div><p class='provider-description'>Local models that serve tool calls are chosen per DM with <code>/model local:NAME</code> or per spawn with <code>local:NAME</code>; see Text models.</p><details class='model-add'><summary>+ New model</summary>");
    for backend in [
        crate::backend::Backend::Codex,
        crate::backend::Backend::Claude,
    ] {
        let id = backend.id();
        let label = backend.label();
        let default = crate::escape(&backend.default_model());
        html.push_str(&format!("<form method='post' action='/providers'><input type='hidden' name='action' value='add'><input type='hidden' name='provider' value='{id}'><label>{label} model ID<input name='model' required maxlength='100' value='{default}' spellcheck='false' autocapitalize='none'></label><button class='primary'>Add model</button></form>"));
    }
    html.push_str("</details></section>");
    html.push_str(&render_text(runtime));
    html.push_str("</div>");
    html
}

/// The Manage → Models page polls this while open: one live server-statistics
/// line per local endpoint, read outside the state lock. Nothing polls when no
/// page is open.
pub fn live_http(stream: &mut impl std::io::Write, state: &std::sync::Mutex<crate::State>, method: &str, path: &str) -> bool {
    if path != "/providers/live" {
        return false;
    }
    let endpoints = match (method, state.lock()) {
        ("GET", Ok(state)) => Ok(local(&state.runtime).cloned().collect::<Vec<_>>()),
        ("GET", Err(_)) => Err("State unavailable"),
        _ => Err("Use GET"),
    };
    let _ = match endpoints {
        Ok(endpoints) => {
            let body = serde_json::Value::Object(crate::model_calls::stats::live(&endpoints)).to_string();
            control_http::respond(stream, "200 OK", "application/json", body.as_bytes(), "", control_http::Policy::Page)
        }
        Err(message) => control_http::respond(stream, "400 Bad Request", "text/plain", message.as_bytes(), "", control_http::Policy::Page),
    };
    true
}

/// Entries naming the same adapter, endpoint and model ID are one model.
fn same_target(a: &crate::model_calls::Endpoint, b: &crate::model_calls::Endpoint) -> bool {
    a.cli == b.cli && a.model == b.model && a.url.trim_end_matches('/') == b.url.trim_end_matches('/')
}
/// Older settings could hold one entry per role for the same model (for example
/// a text-only compactor and a tool-capable agent entry). Collapse each group into
/// one entry: a tool-capable entry's name survives (chats may select it), else the
/// first; it serves tool calls if any duplicate did, and the compactor follows it.
/// Returns the retired names with their replacements so references can follow.
fn merge_duplicates(models: &mut Vec<crate::model_calls::Endpoint>, settings: &mut crate::compaction::Settings) -> Vec<(String, String)> {
    let mut merged: Vec<crate::model_calls::Endpoint> = Vec::with_capacity(models.len());
    let mut renamed: Vec<(String, String)> = Vec::new();
    for model in models.drain(..) {
        match merged.iter_mut().find(|kept| same_target(kept, &model)) {
            None => merged.push(model),
            Some(kept) if model.tools && !kept.tools => {
                let retired = std::mem::replace(kept, model);
                renamed.push((retired.id, kept.id.clone()));
            }
            Some(kept) => renamed.push((model.id, kept.id.clone())),
        }
    }
    // A retired survivor's earlier replacements follow it to the final entry.
    for at in 0..renamed.len() {
        for _ in 0..renamed.len() {
            let Some((_, next)) = renamed.iter().find(|(from, _)| *from == renamed[at].1) else { break };
            renamed[at].1 = next.clone();
        }
    }
    if let Some((_, to)) = renamed.iter().find(|(from, _)| Some(from) == settings.model.as_ref()) {
        settings.model = Some(to.clone());
    }
    *models = merged;
    renamed
}
/// Repoint chat selections of retired local names at their merged entry.
pub(crate) fn repoint(chats: &mut [crate::chat::Session], renamed: &[(String, String)]) -> bool {
    let mut changed = false;
    for model in chats.iter_mut().filter_map(|c| c.model.as_mut()).filter(|m| m.backend == crate::backend::Backend::Local) {
        if let Some((_, to)) = renamed.iter().find(|(from, _)| *from == model.model) {
            model.model = to.clone();
            changed = true;
        }
    }
    changed
}
fn validate_text(models: &[crate::model_calls::Endpoint], settings: &crate::compaction::Settings) -> std::io::Result<()> {
    settings.validate()?;
    if models.len() > 64 || models.iter().enumerate().any(|(at,m)| models[..at].iter().any(|other|other.id==m.id || same_target(other,m))) {
        return Err(std::io::Error::other("Duplicate or too many text models"));
    }
    for model in models { model.validate()?; }
    if settings.enabled && !models.iter().any(|m|Some(&m.id)==settings.model.as_ref()) {
        return Err(std::io::Error::other("Select a configured compactor model before enabling summaries"));
    }
    Ok(())
}
/// The live server-statistics line on a local endpoint's card, filled by the
/// page client from `/providers/live`. CLI adapters have no server to read.
fn live_slot(model: &crate::model_calls::Endpoint) -> String {
    if model.cli.is_some() {
        return String::new();
    }
    format!("<p class='model-stats provider-description' data-live='{}' data-live-fallback='Live statistics unavailable.' aria-live='polite'>Reading live statistics…</p>", crate::escape(&model.id))
}
/// What one entry can do; roles (compactor, generation, agent turns) point at entries.
fn capabilities(model: &crate::model_calls::Endpoint) -> &'static str {
    if model.tools { "summaries · text generation · agent turns" } else { "summaries · text generation" }
}
/// Fields shared by the add and edit forms, so both are parsed and validated alike.
/// Editing keeps the name (chats and the compactor refer to it) and the adapter.
fn text_fields(model: Option<&crate::model_calls::Endpoint>) -> String {
    let value = |f: fn(&crate::model_calls::Endpoint) -> String, default: &str| model.map_or_else(|| default.to_owned(), |m| crate::escape(&f(m)));
    let endpoint = model.is_none_or(|m| m.cli.is_none());
    let mut html = String::from("<div class='provider-fields'>");
    if model.is_none() {
        html.push_str("<label>Adapter<select name='adapter'><option value='endpoint'>Local endpoint</option><option value='codex'>Codex CLI</option><option value='claude'>Claude CLI</option></select></label><label>Name<input name='id' required maxlength='100' placeholder='qwen-local'></label>");
    }
    html.push_str(&format!("<label>Model ID or server alias<input name='model' required maxlength='100' value='{}'></label>", value(|m| m.model.clone(), "")));
    if endpoint {
        html.push_str(&format!("<label>Private endpoint{}<input name='url' placeholder='http://127.0.0.1:8081/v1' value='{}'></label>", if model.is_none() { " (leave empty for CLI)" } else { "" }, value(|m| m.url.clone(), "")));
    }
    html.push_str(&format!(concat!("<label>Weights/runtime revision<input name='revision' required maxlength='256' value='{}'></label>",
        "<label>Declared context tokens<input name='context_tokens' type='number' min='1024' value='{}'></label><label>Output token reserve<input name='output_tokens' type='number' min='1' value='{}'></label>",
        "<label>Input byte ceiling<input name='input_bytes' type='number' min='1' max='1048576' value='{}'></label><label>Output byte ceiling<input name='output_bytes' type='number' min='1' max='65536' value='{}'></label>",
        "<label>Request timeout seconds<input name='timeout_seconds' type='number' min='1' max='86400' value='{}'></label></div>"),
        value(|m| m.revision.clone(), ""), value(|m| m.limits.context_tokens.to_string(), "32768"), value(|m| m.limits.output_tokens.to_string(), "1024"),
        value(|m| m.limits.input_bytes.to_string(), "28000"), value(|m| m.limits.output_bytes.to_string(), "8192"), value(|m| m.timeout_seconds.to_string(), "1800")));
    if endpoint {
        html.push_str(&format!("<label class='summary-toggle'><input name='tools' type='checkbox'{}><span><strong>Serves tool calls</strong><small>Local endpoint only. Declares OpenAI-style tool calls (llama.cpp <code>--jinja</code>) so DMs and agents may select <code>local:NAME</code> for agent turns.</small></span></label>", if model.is_some_and(|m| m.tools) { " checked" } else { "" }));
    }
    html
}
fn render_text(runtime: &crate::runtime::Runtime) -> String {
    let memory = &runtime.compaction;
    let mut html = "<section class='provider-section' aria-labelledby='summary-models-heading'><h3 id='summary-models-heading'>Text models</h3><p class='provider-description'>One entry per endpoint and model. Each can summarize history and answer <code>control_model_generate</code>; a local endpoint that serves tool calls also runs agent turns. The compactor and per-DM agent choices point at these entries.</p><div class='model-list' aria-label='Text models'".to_owned();
    // While this page is open its client reads each local server's statistics
    // every few seconds; a closed or hidden page reads nothing.
    if local(runtime).next().is_some() {
        html.push_str(" data-live-src='/providers/live' data-live-every='5'");
    }
    html.push('>');
    if memory.models.is_empty() {
        html.push_str("<p class='provider-empty'>No text models configured. Add one to enable Control compaction or local generation.</p>");
    }
    for model in &memory.models {
        let adapter = model.cli.map_or("Local endpoint", |backend| backend.label());
        let id = crate::escape(&model.id);
        let compactor = if memory.settings.enabled && memory.settings.model.as_deref() == Some(model.id.as_str()) { "<span class='model-default'>Compactor</span>" } else { "" };
        html.push_str(&format!("<article class='model-row summary-model'><div class='model-card-heading'><div class='model-name'><h4>{id}</h4><span class='model-provider'>{} · {adapter} · {}</span></div>{compactor}<form method='post' action='/providers'><input type='hidden' name='action' value='text-remove'><input type='hidden' name='id' value='{id}'><button class='provider-secondary' aria-label='Remove text model {id}'>Remove</button></form></div><dl class='model-metadata'><div><dt>Context tokens</dt><dd>{}</dd></div><div><dt>Output reserve</dt><dd>{} tokens</dd></div></dl>{}<details class='model-revision'><summary>Weights / runtime revision</summary><code>{}</code></details><details class='model-revision text-edit'><summary aria-label='Edit text model {id}'>Edit</summary><form method='post' action='/providers'><input type='hidden' name='action' value='text-edit'><input type='hidden' name='id' value='{id}'>{}<button class='primary'>Save text model</button></form></details></article>", crate::escape(&model.model), capabilities(model), model.limits.context_tokens, model.limits.output_tokens, live_slot(model), crate::escape(&model.revision), text_fields(Some(model))));
    }
    html.push_str(&format!("</div><details class='model-add'><summary>+ New text model</summary><form method='post' action='/providers'><input type='hidden' name='action' value='text-add'>{}<button class='primary'>Add text model</button></form></details></section>", text_fields(None)));
    html.push_str("<section class='provider-section' aria-labelledby='compaction-heading'><h3 id='compaction-heading'>History compaction</h3><p class='provider-description'>Opt in per DM. Original messages remain available in the history tree.</p><form class='compaction-settings' method='post' action='/providers'><input type='hidden' name='action' value='compaction'>");
    html.push_str(&format!("<label class='summary-toggle'><input name='enabled' type='checkbox'{}><span><strong>Automatic summaries</strong><small>Use Control compaction for the DMs listed below.</small></span></label><div class='provider-fields'><label>Summary model<select name='model'><option value=''>Select a summary model</option>",if memory.settings.enabled {" checked"}else{""}));
    for model in &memory.models { html.push_str(&format!("<option value='{}'{}>{}</option>",crate::escape(&model.id),if Some(&model.id)==memory.settings.model.as_ref(){" selected"}else{""},crate::escape(&model.id))); }
    html.push_str(&format!("</select></label><label>DMs to summarize<input name='chats' maxlength='8192' placeholder='c-example c-another' value='{}'><small>Space-separated DM IDs. Empty means none.</small></label></div>",crate::escape(&memory.settings.chats.join(" "))));
    html.push_str(&format!("<details class='compaction-advanced'><summary>Advanced window settings</summary><div class='provider-fields'><label>Recent entries kept verbatim<input name='recent_entries' type='number' min='1' max='128' value='{}'></label><label>Leaf chunk entries (power of two)<input name='leaf_entries' type='number' min='1' max='64' value='{}'></label><label>History byte budget<input name='history_bytes' type='number' min='1024' max='262144' value='{}'></label></div><p class='provider-description'>Input is conservatively byte-bounded using declared context/output limits. Requests exceeding capacity fail visibly.</p></details><p class='provider-description'>Inspect a DM at <code>/dm/ID/compaction</code>, or use <code>/compact</code> in the TUI.</p><div class='provider-actions'><button class='primary'>Save compaction settings</button></div></form></section>", memory.settings.recent_entries,memory.settings.leaf_entries,memory.settings.history_bytes));
    html
}
fn save_text(state: &mut crate::State, fields: &HashMap<String,String>, save: impl FnOnce(&crate::State)->std::io::Result<()>) -> Result<(), &'static str> {
    let mut models = state.runtime.compaction.models.clone();
    let mut settings = state.runtime.compaction.settings.clone();
    let get = |name| fields.get(name).map(String::as_str).unwrap_or("");
    // Add and edit parse the same fields; edit keeps the entry's name and adapter.
    let parse = |id:&str,cli| -> Result<crate::model_calls::Endpoint,&'static str> { Ok(crate::model_calls::Endpoint { id:id.into(),model:get("model").trim().into(),url:get("url").trim().into(),revision:get("revision").into(),cli,
        limits:control_ai::model::Limits{context_tokens:get("context_tokens").parse().map_err(|_|"Invalid context size")?,output_tokens:get("output_tokens").parse().map_err(|_|"Invalid output size")?,
            input_bytes:get("input_bytes").parse().map_err(|_|"Invalid input byte ceiling")?,output_bytes:get("output_bytes").parse().map_err(|_|"Invalid output byte ceiling")?},
        timeout_seconds:get("timeout_seconds").parse().map_err(|_|"Invalid request timeout")?,
        tools:fields.contains_key("tools") }) };
    match get("action") {
        "text-add" => {
            let cli = match get("adapter") {""|"endpoint"=>None,"codex"=>Some(crate::backend::Backend::Codex),"claude"=>Some(crate::backend::Backend::Claude),_=>return Err("Unknown text adapter")};
            models.push(parse(get("id"),cli)?);
        }
        "text-edit" => {
            let index = models.iter().position(|m|m.id==get("id")).ok_or("This text model changed. Reload the list.")?;
            models[index] = parse(get("id"),models[index].cli)?;
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
    if models.iter().enumerate().any(|(at,m)|models[..at].iter().any(|other|other.id!=m.id && same_target(other,m))) {
        return Err("Another entry already uses this endpoint and model ID. Edit that entry instead: one entry serves summaries, generation and agent turns.");
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
