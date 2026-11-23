//! Personas on this node: the library (versioned like genes), capture into a
//! run's lineage, and the one place a run's standing is read. Every
//! standing-dependent check (judge tool policy, who may be steered, operator
//! notices, node visibility) asks [`of`]. A persona composes existing pieces and
//! never grants access: each standing is a host profile that keeps its own gates
//! (opt-in genes, operator bot approval, tree position, actor).
//!
//! Runs without a captured persona keep their pre-persona behaviour exactly: their
//! standing is the shipped built-in for their tree position and approval (`lead`
//! at the root, `worker` below, and for read-only judges `antibody`, `advisor` or
//! `judge`). See the adjacent tests.
use crate::{State, escape};
use control_context::persona::{self, Acts, AnswersTo, Behaviour, Captured, Definition, Sees, Standing, Steers};
use serde_json::{Value, json};
use std::{collections::HashMap, io};

/// A run's standing as enforcement sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Effective {
    /// The persona ID (captured, or the built-in its position implies).
    pub id: String,
    pub standing: Standing,
    /// The run's chat is an approved persistent observer (advisor or antibody).
    pub approved: bool,
    pub captured: bool,
}
impl Effective {
    /// The shipped built-in `id`'s standing, for runs without a captured persona.
    pub(crate) fn builtin(id: &str, approved: bool) -> Self {
        Self {
            id: id.into(),
            standing: persona::builtin(id).map(|d| d.standing).unwrap_or_default(),
            approved,
            captured: false,
        }
    }
    pub(crate) fn read_only(&self) -> bool {
        self.standing.acts.read_only()
    }
    /// Host tool policy. A read-only persona keeps the judge set; its notes
    /// and notices (`Judge::Advisor` tools) need operator approval as a bot.
    pub(crate) fn may_call(&self, name: &str) -> bool {
        match &self.standing.acts {
            Acts::Full => true,
            Acts::ReadOnly => control_context::judge_may_call(name, self.approved),
            Acts::Deny(tools) => !tools.iter().any(|tool| same_tool(tool, name)),
        }
    }
    /// An approved node observer: reads any team and notes other teams' leads.
    pub(crate) fn antibody(&self) -> bool {
        self.read_only() && self.approved && self.standing.sees == Sees::Node && self.standing.steers == Steers::Leads
    }
    /// An approved team observer: notes its own team's lead.
    pub(crate) fn advisor(&self) -> bool {
        self.read_only() && self.approved && self.standing.steers == Steers::Lead
    }
    /// Whether it may escalate to the operator with a notice; the opt-in
    /// gene, tree position and rate limits are checked by the notice itself.
    pub(crate) fn notifies(&self) -> bool {
        self.standing.answers_to == AnswersTo::Operator && (!self.read_only() || self.approved)
    }
}
/// `control_service_start`, `control.service.start` and `service_start` name one tool.
fn same_tool(listed: &str, name: &str) -> bool {
    let normal = |s: &str| {
        s.trim_start_matches("control")
            .trim_start_matches(['_', '.'])
            .replace('.', "_")
    };
    listed == name || normal(listed) == normal(name)
}

/// The standing of `run`: its captured persona, else the built-in its tree
/// position and approval imply (pre-persona behaviour, unchanged).
pub(crate) fn of(bots: &crate::bots::Bots, run: &crate::runtime::Run) -> Effective {
    let chat = run.chat_id.as_deref().unwrap_or("");
    let judge = crate::delegation::is_judge(run);
    let approved = judge && !chat.is_empty() && bots.advises(chat);
    if let Some(bundle) = run.persona.as_ref().and_then(|p| p.bundle.as_deref()) {
        return Effective {
            id: bundle.id.clone(),
            standing: bundle.standing.clone(),
            approved,
            captured: true,
        };
    }
    let id = if judge {
        if !chat.is_empty() && bots.antibody(chat) {
            "antibody"
        } else if approved {
            "advisor"
        } else {
            "judge"
        }
    } else if run.persona.as_ref().is_some_and(|p| p.depth > 0) {
        "worker"
    } else {
        "lead"
    };
    Effective::builtin(id, approved)
}
/// The persona captured by `chat`'s latest run, if any.
pub(crate) fn captured<'a>(runs: &'a [crate::runtime::Run], chat: &str) -> Option<&'a Captured> {
    runs.iter()
        .rev()
        .find(|run| run.chat_id.as_deref() == Some(chat))
        .and_then(|run| run.persona.as_ref())
        .and_then(|p| p.bundle.as_deref())
}
/// Whether `chat`'s approved judge observes the whole node (an antibody) rather
/// than its team: its captured persona's visibility, else whether the root
/// opted in to antibodies (pre-persona behaviour).
pub(crate) fn observes_node(state: &State, chat: &str) -> bool {
    match captured(&state.runtime.runs, chat) {
        Some(bundle) => bundle.standing.sees == Sees::Node,
        None => crate::bots::antibody_offered_for(state, chat),
    }
}
/// The opt-in gate for an observer persona in `chat`'s tree: node observers
/// need the antibody gene on an operator-owned root, team observers one of the
/// team-observer genes (each kind's own gene is checked where it starts). `None` for runs without a captured persona (their legacy gate applies).
pub(crate) fn observer_offered(state: &State, chat: &str) -> Option<bool> {
    captured(&state.runtime.runs, chat)
        .filter(|b| b.standing.acts.read_only())
        .map(|bundle| match bundle.standing.sees {
            Sees::Node => crate::bots::antibody_offered_for(state, chat),
            Sees::Team => crate::bots::team_observer_offered_for(state, chat),
        })
}
/// The schedule `chat`'s persona asks for: the default `every_minutes` of a bot
/// request for it.
pub(crate) fn schedule(runs: &[crate::runtime::Run], chat: &str) -> Option<u64> {
    captured(runs, chat)?.behaviours.iter().find_map(|b| match b {
        Behaviour::Schedule { every_minutes } => Some(*every_minutes),
        _ => None,
    })
}

/// Capture persona `id`'s current revision, with its SOUL.md resolved.
pub(crate) fn capture(state: &State, id: &str) -> Result<Captured, String> {
    let definition = state
        .personas
        .get(id.trim())
        .ok_or_else(|| format!("Unknown persona: {id}. control.personas.list names them."))?;
    Captured::of(definition, &crate::lens_library::inputs(&state.documents))
}

/// Validate genes, SOUL.md and the library, then append `draft` as the next
/// revision. Persist failure restores the previous library.
pub(crate) fn save_with(
    state: &mut State,
    mut draft: Definition,
    persist: impl FnOnce(&State) -> io::Result<()>,
) -> Result<Definition, String> {
    if state.personas.get(&draft.id).map_or(0, |d| d.revision) != draft.revision {
        return Err(
            "This persona changed in another tab. Your edit is below; open the latest revision before merging.".into(),
        );
    }
    draft.revision = draft.revision.checked_add(1).ok_or("Revision limit reached.")?;
    draft.lenses.sort();
    draft.lenses.dedup();
    if let Acts::Deny(tools) = &mut draft.standing.acts {
        tools.sort();
        tools.dedup();
    }
    crate::lens_library::resolve(state, &draft.lenses)?;
    Captured::of(&draft, &crate::lens_library::inputs(&state.documents))?;
    let mut candidate = state.personas.clone();
    candidate.definitions.push(draft.clone());
    candidate.validate()?;
    let previous = std::mem::replace(&mut state.personas, candidate);
    if persist(state).is_err() {
        state.personas = previous;
        return Err("Could not save to disk. Your edit is below.".into());
    }
    Ok(draft)
}

/// Select `persona` (or `None`) for a root DM's future messages. The DM's DNA
/// becomes the persona's genes, which the gene picker may still change.
pub(crate) fn select(state: &mut State, chat: &str, persona: Option<&str>) -> Result<(), &'static str> {
    let captured = match persona {
        Some(id) => Some(
            capture(state, id)
                .map_err(|_| "Unknown persona or its SOUL.md cannot be captured; control.personas.list names them.")?,
        ),
        None => None,
    };
    if captured.as_ref().is_some_and(|c| c.standing.acts.read_only()) {
        return Err("Read-only personas run as spawned observers (control_agent_spawn with persona), not as a DM.");
    }
    if let Some(captured) = &captured {
        crate::lens_library::resolve(state, &captured.lenses)
            .map_err(|_| "The persona's genes cannot be resolved; check its DNA.")?;
    }
    let session = state.chats.iter_mut().find(|s| s.id == chat).ok_or("Chat not found.")?;
    crate::identity::authorize(&session.actor, crate::identity::Action::Task)?;
    if session.parent_chat.is_some() {
        return Err("Select a persona on a root DM; spawned agents take theirs at spawn.");
    }
    session.persona = captured.as_ref().map(|c| c.id.clone());
    if let Some(captured) = captured {
        session.lens = (captured.lenses.len() == 1).then(|| captured.lenses[0].clone());
        session.lenses = Some(captured.lenses);
    }
    session.revision = session.revision.saturating_add(1);
    Ok(())
}
/// Applies a root DM's selected persona to its newly queued run: the captured
/// purpose and standing, and its body when the DM has no explicit model.
pub(crate) fn apply_to_root(state: &mut State, chat: &str, run_index: usize) -> Result<(), &'static str> {
    let Some(id) = state
        .chats
        .iter()
        .find(|s| s.id == chat)
        .and_then(|s| s.persona.clone())
    else {
        return Ok(());
    };
    let bundle =
        capture(state, &id).map_err(|_| "The DM's persona cannot be captured; check it on the Personas page.")?;
    let explicit = state
        .chats
        .iter()
        .find(|s| s.id == chat)
        .is_some_and(|s| s.model.is_some());
    let run = &state.runtime.runs[run_index];
    let body = if !explicit && !bundle.model.is_empty() {
        Some(crate::providers::selection::resolve(
            &state.runtime,
            run.backend,
            &run.model.clone().unwrap_or_else(|| run.backend.default_model()),
            Some(&bundle.model),
        )?)
    } else {
        None
    };
    let run = &mut state.runtime.runs[run_index];
    if let Some(body) = body {
        run.backend = body.backend;
        run.model = Some(body.model);
    }
    run.persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Worker,
        root: run.id.clone(),
        depth: 0,
        parent: None,
        bundle: Some(Box::new(bundle)),
    });
    Ok(())
}

/// Operator tools: `control.personas.list`, `control.persona.read` and
/// `control.chat.persona`.
pub(crate) fn tool(state: &mut State, name: &str, arguments: &Value) -> Result<Value, &'static str> {
    match name {
        "control.personas.list" => Ok(json!(state.personas.current())),
        "control.persona.read" => {
            let id = arguments["id"].as_str().ok_or("Missing persona ID.")?;
            let definition = match arguments["revision"].as_u64() {
                Some(revision) => state.personas.revision(id, revision),
                None => state.personas.get(id),
            }
            .ok_or("Persona not found.")?;
            Ok(
                json!({"definition":definition,"revisions":state.personas.history(id).iter().map(|d| d.revision).collect::<Vec<_>>(),"standing":definition.standing.describe()}),
            )
        }
        "control.chat.persona" => {
            let chat = arguments["id"]
                .as_str()
                .filter(|id| crate::chat::valid_id(id))
                .ok_or("Invalid chat ID.")?;
            let persona = match arguments.get("persona") {
                None => return Err("Specify a persona, or null to clear it."),
                Some(Value::Null) => None,
                Some(Value::String(s)) if s == "none" || s.is_empty() => None,
                Some(Value::String(s)) => Some(s.as_str()),
                Some(_) => return Err("Expected a persona ID."),
            };
            select(state, chat, persona)?;
            crate::chat::detail(state, chat)
        }
        _ => Err("Unknown persona tool."),
    }
}

// --- Personas page (server-rendered, beside DNA) ---

fn option<T: PartialEq>(value: &str, label: &str, current: T, this: T) -> String {
    format!(
        "<option value='{value}'{}>{label}</option>",
        if current == this { " selected" } else { "" }
    )
}
pub(crate) fn draft(fields: &HashMap<String, String>) -> Result<Definition, String> {
    let value = |key: &str| fields.get(key).cloned().unwrap_or_default();
    let sees = match value("sees").as_str() {
        "node" => Sees::Node,
        _ => Sees::Team,
    };
    let steers = match value("steers").as_str() {
        "none" => Steers::None,
        "lead" => Steers::Lead,
        "leads" => Steers::Leads,
        _ => Steers::Team,
    };
    let answers_to = match value("answers_to").as_str() {
        "operator" => AnswersTo::Operator,
        _ => AnswersTo::Lead,
    };
    let deny = value("deny")
        .split([',', ' ', '\n', '\r'])
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let acts = match value("acts").as_str() {
        "read_only" => Acts::ReadOnly,
        "deny" => Acts::Deny(deny),
        _ => Acts::Full,
    };
    let behaviours = value("behaviours");
    let behaviours = if behaviours.trim().is_empty() {
        Vec::new()
    } else {
        serde_json::from_str(&behaviours).map_err(|_| {
            "Behaviours must be a JSON list such as [{\"kind\":\"schedule\",\"every_minutes\":30}].".to_owned()
        })?
    };
    Ok(Definition {
        id: value("id"),
        name: value("name"),
        revision: value("revision").parse().unwrap_or(0),
        purpose: value("purpose").trim().to_owned(),
        soul: value("soul"),
        lenses: fields
            .keys()
            .filter_map(|k| k.strip_prefix("gene_").map(str::to_owned))
            .collect(),
        standing: Standing {
            sees,
            steers,
            answers_to,
            acts,
        },
        model: value("model").trim().to_owned(),
        behaviours,
    })
}
pub(crate) fn render(state: &State, id: &str, submitted: Option<&Definition>, notice: Option<(&str, bool)>) -> String {
    let blank = Definition::default();
    let definition = submitted.or_else(|| state.personas.get(id)).unwrap_or(&blank);
    let mut html = String::from(
        "<div class='personas'><div class='section-heading'><h2>Personas</h2><a class='primary' href='/personas?new=1'>＋ New persona</a></div><p class='muted'>A persona is a purpose (its SOUL), DNA, standing, a default model and opt-in behaviours. Spawn one with <code>control_agent_spawn {persona}</code> or select it for a root DM (<code>/persona NAME</code>). Runs capture the revision they start with.</p><table class='persona-table'><thead><tr><th>Persona</th><th>DNA</th><th>Standing</th><th>Body</th></tr></thead><tbody>",
    );
    for persona in state.personas.current() {
        html.push_str(&format!(
            "<tr{}><td><a href='/personas?id={}'>{}</a> <small>v{}</small></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            if persona.id == definition.id {
                " aria-current='true'"
            } else {
                ""
            },
            escape(&persona.id),
            escape(&persona.name),
            persona.revision,
            persona
                .lenses
                .iter()
                .map(|l| format!("<a href='/dna?id={}'>{}</a>", escape(l), escape(l)))
                .collect::<Vec<_>>()
                .join(" + "),
            escape(&persona.standing.describe()),
            if persona.model.is_empty() {
                "inherits".to_owned()
            } else {
                escape(&persona.model)
            }
        ));
    }
    html.push_str("</tbody></table>");
    if let Some((message, saved)) = notice {
        html.push_str(&format!(
            "<p class='{}' role='{}'>{}</p>",
            if saved { "save-confirmation" } else { "save-error" },
            if saved { "status" } else { "alert" },
            escape(message)
        ));
    }
    let existing = state.personas.get(&definition.id).is_some();
    html.push_str(&format!("<form class='lens-definition persona-definition' method='post' action='/personas/save'><input type='hidden' name='revision' value='{}'><div class='lens-name-row'><label>Name<input name='name' required maxlength='80' value='{}' placeholder='Shopkeeper'></label><label>Identifier<input name='id' required maxlength='64' pattern='[a-z0-9-]+' value='{}' placeholder='shopkeeper'{}></label></div>",
        definition.revision, escape(&definition.name), escape(&definition.id), if existing { " readonly" } else { "" }));
    html.push_str(&format!("<label>Purpose (SOUL)<textarea name='purpose' rows='3' maxlength='{}' placeholder='Why this persona exists, in a few sentences'>{}</textarea></label>", persona::MAX_PURPOSE, escape(&definition.purpose)));
    html.push_str("<label>SOUL.md document<select name='soul'><option value=''>None: use the purpose above</option>");
    for doc in &state.documents {
        html.push_str(&format!(
            "<option value='{}'{}>{}</option>",
            escape(&doc.id),
            if doc.id == definition.soul { " selected" } else { "" },
            escape(&doc.title)
        ));
    }
    html.push_str("</select></label><fieldset><legend>DNA</legend><div class='library-choices'>");
    for gene in state.lens_library.current() {
        html.push_str(&format!(
            "<label class='library-choice'><input type='checkbox' name='gene_{}'{}><span>{}</span></label>",
            escape(&gene.id),
            if definition.lenses.contains(&gene.id) {
                " checked"
            } else {
                ""
            },
            escape(&gene.name)
        ));
    }
    let s = &definition.standing;
    let acts = match &s.acts {
        Acts::Full => "full",
        Acts::ReadOnly => "read_only",
        Acts::Deny(_) => "deny",
    };
    html.push_str("</div></fieldset><fieldset><legend>Standing</legend><p class='muted'>Host profiles, not grants: observers still need their gene on the root and operator approval.</p><div class='lens-name-row'>");
    html.push_str(&format!(
        "<label>Sees<select name='sees'>{}{}</select></label>",
        option("team", "Own team", s.sees, Sees::Team),
        option("node", "Node (antibody)", s.sees, Sees::Node)
    ));
    html.push_str(&format!(
        "<label>Steers<select name='steers'>{}{}{}{}</select></label>",
        option("team", "Its team", s.steers, Steers::Team),
        option("none", "No one", s.steers, Steers::None),
        option("lead", "Its lead (notes)", s.steers, Steers::Lead),
        option("leads", "Other teams' leads (notes)", s.steers, Steers::Leads)
    ));
    html.push_str(&format!(
        "<label>Answers to<select name='answers_to'>{}{}</select></label>",
        option("lead", "Its lead", s.answers_to, AnswersTo::Lead),
        option("operator", "The operator", s.answers_to, AnswersTo::Operator)
    ));
    html.push_str(&format!(
        "<label>Acts<select name='acts'>{}{}{}</select></label></div>",
        option("full", "Full worker tools", acts, "full"),
        option("read_only", "Read-only judge set", acts, "read_only"),
        option("deny", "Full minus deny list", acts, "deny")
    ));
    html.push_str(&format!("<label>Deny list<input name='deny' value='{}' placeholder='control_service_start, control_agent_spawn'></label></fieldset>", escape(&match &definition.standing.acts { Acts::Deny(tools) => tools.join(", "), _ => String::new() })));
    html.push_str(&format!("<label>Body (default model)<input name='model' maxlength='128' value='{}' placeholder='Empty inherits; e.g. claude:opus or local:NAME'></label>", escape(&definition.model)));
    html.push_str(&format!("<label>Behaviours<textarea name='behaviours' rows='3' placeholder='[{{\"kind\":\"schedule\",\"every_minutes\":30}}, {{\"kind\":\"observe\",\"command\":\"cat status.json\"}}]'>{}</textarea></label><p class='muted'>Opt-in standing orders over existing primitives: schedule (bot timer), room (wake on posts), watch (routine with optional salience), observe, score. Behaviour trees and state machines plug in here later.</p><button class='primary'>Save persona ↗</button></form>",
        escape(&if definition.behaviours.is_empty() { String::new() } else { serde_json::to_string(&definition.behaviours).unwrap_or_default() })));
    if existing && submitted.is_none() {
        html.push_str("<details class='resolved-context' open><summary>First instruction</summary>");
        match capture(state, &definition.id) {
            Ok(captured) => html.push_str(&format!("<pre class='manifest'>{}</pre>", escape(&captured.prompt()))),
            Err(error) => html.push_str(&format!("<p class='save-error'>{}</p>", escape(&error))),
        }
        let revisions = state.personas.history(&definition.id);
        html.push_str(&format!(
            "<p class='muted'>Revisions: {}</p></details>",
            revisions
                .iter()
                .map(|d| format!("v{}", d.revision))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    html.push_str("</div>");
    html
}
pub(crate) fn handle(
    stream: &mut impl io::Write,
    state: &mut State,
    method: &str,
    path: &str,
    query: &HashMap<String, String>,
    lens: &str,
    body: &[u8],
) -> bool {
    if path == "/personas" && method == "GET" {
        let id = if query.contains_key("new") {
            ""
        } else {
            query.get("id").map(String::as_str).unwrap_or("lead")
        };
        let saved = query
            .get("saved")
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|v| state.personas.get(id).is_some_and(|d| d.revision == v));
        let content = render(
            state,
            id,
            None,
            saved.then_some(("✓ Persona saved. New spawns and messages capture this revision.", true)),
        );
        let _ = control_http::respond(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            crate::render(state, "persona-page", lens, &content).as_bytes(),
            "",
            control_http::Policy::Page,
        );
        return true;
    }
    if path == "/personas/save" && method == "POST" {
        let fields = crate::fields(&String::from_utf8_lossy(body));
        let result = draft(&fields).and_then(|d| save_with(state, d, crate::storage::save));
        match result {
            Ok(d) => {
                let _ = control_http::respond(
                    stream,
                    "303 See Other",
                    "text/plain",
                    b"",
                    &format!("Location: /personas?id={}&saved={}\r\n", d.id, d.revision),
                    control_http::Policy::Page,
                );
            }
            Err(error) => {
                let submitted = draft(&fields).ok();
                let html = render(
                    state,
                    fields.get("id").map(String::as_str).unwrap_or(""),
                    submitted.as_ref(),
                    Some((&error, false)),
                );
                let _ = control_http::respond(
                    stream,
                    "409 Conflict",
                    "text/html; charset=utf-8",
                    crate::render(state, "persona-page", lens, &html).as_bytes(),
                    "",
                    control_http::Policy::Page,
                );
            }
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests;
