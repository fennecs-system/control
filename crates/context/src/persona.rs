//! Personas: named, versioned compositions of existing pieces. A persona is a
//! purpose (its SOUL: why it exists), DNA (genes), standing (what it may see,
//! whom it may steer, whom it answers to, what it may act on), a body (its
//! default model) and behaviours (opt-in standing orders over existing
//! primitives). Pure data and validation; resolution against the node, capture
//! and enforcement live in the server. A persona never grants access: every
//! standing is one of the host's existing profiles and keeps its host gates.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Stored definitions, all revisions included.
const MAX_DEFINITIONS: usize = 1024;
/// Distinct current personas.
const MAX_PERSONAS: usize = 64;
/// Characters in an inline purpose. Keep it short: it is the first instruction.
pub const MAX_PURPOSE: usize = 600;
/// Characters of a SOUL.md document captured as a purpose.
pub const MAX_SOUL: usize = 2000;
/// Behaviours per persona.
pub const MAX_BEHAVIOURS: usize = 8;

/// What a persona may see.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sees {
    /// Its own task tree.
    #[default]
    Team,
    /// Every team on the node (an approved antibody only).
    Node,
}
/// Whom a persona may steer with `control_agent_message`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Steers {
    /// Nobody: it reports through its result only.
    None,
    /// Its descendants and fellow workers in its own task (the peer rule).
    #[default]
    Team,
    /// Its own team's lead, with evidence notes (an approved advisor).
    Lead,
    /// Other teams' leads, with evidence notes (an approved antibody).
    Leads,
}
/// Whom a persona answers to and escalates to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswersTo {
    /// Its team's lead.
    #[default]
    Lead,
    /// The operator: design questions and, when opted in, operator notices.
    Operator,
}
/// What a persona may act on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Acts {
    /// The full worker tool set its tree position allows.
    #[default]
    Full,
    /// The read-only judge set (a judge, advisor or antibody).
    ReadOnly,
    /// The full set minus these worker tools.
    Deny(Vec<String>),
}
impl Acts {
    pub fn read_only(&self) -> bool {
        matches!(self, Acts::ReadOnly)
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Standing {
    pub sees: Sees,
    pub steers: Steers,
    pub answers_to: AnswersTo,
    pub acts: Acts,
}
impl Standing {
    /// One line for prompts and listings.
    pub fn describe(&self) -> String {
        let sees = match self.sees {
            Sees::Team => "its own team",
            Sees::Node => "every team on the node",
        };
        let steers = match self.steers {
            Steers::None => "no one",
            Steers::Team => "its descendants and fellow workers",
            Steers::Lead => "its team's lead (evidence notes)",
            Steers::Leads => "other teams' leads (evidence notes)",
        };
        let answers = match self.answers_to {
            AnswersTo::Lead => "its lead",
            AnswersTo::Operator => "the operator",
        };
        let acts = match &self.acts {
            Acts::Full => "full worker tools".to_owned(),
            Acts::ReadOnly => "read-only judge tools".to_owned(),
            Acts::Deny(tools) => format!("worker tools except {}", tools.join(", ")),
        };
        format!("sees {sees}; steers {steers}; answers to {answers}; acts with {acts}")
    }
    fn valid(&self) -> bool {
        let read_only = self.acts.read_only();
        let deny = match &self.acts {
            Acts::Deny(tools) => {
                !tools.is_empty() && tools.len() <= 64 && tools.iter().all(|t| !t.is_empty() && t.len() <= 64)
            }
            _ => true,
        };
        // Node visibility and lead steering belong to read-only observers;
        // acting personas keep the team peer rule.
        deny && match (self.sees, self.steers) {
            (Sees::Node, Steers::Leads | Steers::None) => read_only,
            (Sees::Node, _) => false,
            (Sees::Team, Steers::Leads) => false,
            (Sees::Team, Steers::Lead) => read_only,
            (Sees::Team, Steers::Team) => !read_only,
            (Sees::Team, Steers::None) => true,
        }
    }
}

/// An opt-in standing order composed only from existing primitives. Applied
/// when the persona is instantiated (spawn or bot approval), visible in the
/// captured persona, and checked by the same host operations as a direct call.
/// Richer routines (behaviour trees, state machines) and hooks plug into this
/// slot later (roadmap W1/W2/H1); there is no engine here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Behaviour {
    /// Wake every N minutes: files the bot request `control_bot_request
    /// {every_minutes}` would, for the operator to approve.
    Schedule { every_minutes: u64 },
    /// Wake on the team room's posts (a bot's room subscription).
    Room,
    /// A routine: `control_watch_start` in the persona's service.
    Watch {
        name: String,
        command: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        salience: String,
    },
    /// A perception source: `control_observe_set` with this command.
    Observe { command: String },
    /// A progress measure: `control_score_probe`.
    Score { name: String, command: String },
}
impl Behaviour {
    fn valid(&self) -> bool {
        let text = |s: &str, max: usize| !s.trim().is_empty() && s.chars().count() <= max;
        match self {
            Behaviour::Schedule { every_minutes } => (1..=1440).contains(every_minutes),
            Behaviour::Room => true,
            Behaviour::Watch {
                name,
                command,
                salience,
            } => text(name, 48) && text(command, 4000) && salience.chars().count() <= 1000,
            Behaviour::Observe { command } => text(command, 4000),
            Behaviour::Score { name, command } => text(name, 48) && text(command, 4000),
        }
    }
    /// One line naming the existing primitive it composes.
    pub fn describe(&self) -> String {
        match self {
            Behaviour::Schedule { every_minutes } => {
                format!("schedule: wake every {every_minutes} min (bot timer, operator approval)")
            }
            Behaviour::Room => "wake on room posts (bot room subscription, operator approval)".into(),
            Behaviour::Watch {
                name,
                command,
                salience,
            } => format!(
                "routine {name}: control_watch_start {{name:{name:?}, command:{command:?}{}}}",
                if salience.is_empty() {
                    String::new()
                } else {
                    format!(", salience:{salience}")
                }
            ),
            Behaviour::Observe { command } => format!("observation: control_observe_set {{command:{command:?}}}"),
            Behaviour::Score { name, command } => {
                format!("score probe {name}: control_score_probe {{name:{name:?}, command:{command:?}}}")
            }
        }
    }
    /// Whether it needs a service the persona starts or was granted.
    pub fn needs_service(&self) -> bool {
        matches!(
            self,
            Behaviour::Watch { .. } | Behaviour::Observe { .. } | Behaviour::Score { .. }
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub revision: u64,
    /// The SOUL: why this persona exists, shown as its first instruction.
    pub purpose: String,
    /// A knowledge document authored as SOUL.md; when set, its captured body
    /// is the purpose instead.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub soul: String,
    /// DNA: the genes (lenses) its runs resolve.
    pub lenses: Vec<String>,
    pub standing: Standing,
    /// Body: the default model selector (`claude:opus`, `local:NAME`, …);
    /// empty inherits the spawning chat's model.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub behaviours: Vec<Behaviour>,
}
impl Definition {
    fn valid(&self) -> bool {
        crate::valid_id(&self.id)
            && !self.name.trim().is_empty()
            && self.name.chars().count() <= 80
            && self.revision > 0
            && self.purpose.chars().count() <= MAX_PURPOSE
            && self.soul.len() <= 128 && !self.soul.contains(char::is_whitespace)
            && self.lenses.len() <= 32
            && self.lenses.iter().all(|l| crate::valid_id(l))
            && self.model.len() <= 128
            && self.standing.valid()
            && self.behaviours.len() <= MAX_BEHAVIOURS
            && self.behaviours.iter().all(Behaviour::valid)
            // An advisor wakes only on its team's facts; other observers may patrol.
            && !(self.standing.steers == Steers::Lead && self.behaviours.iter().any(|b| matches!(b, Behaviour::Schedule { .. } | Behaviour::Room)))
            // Read-only personas start no routines, probes or observations.
            && !(self.standing.acts.read_only() && self.behaviours.iter().any(Behaviour::needs_service))
    }
}

/// What a run captured: the definition revision with its purpose resolved
/// (a SOUL.md body is copied in), so later edits never change running work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Captured {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub purpose: String,
    /// The SOUL.md document and revision the purpose came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soul: Option<(String, u64)>,
    pub lenses: Vec<String>,
    pub standing: Standing,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub behaviours: Vec<Behaviour>,
}
impl Captured {
    /// Capture `definition`, resolving its SOUL.md from `documents`.
    pub fn of(definition: &Definition, documents: &[crate::DocumentInput]) -> Result<Self, String> {
        let (purpose, soul) = if definition.soul.is_empty() {
            (definition.purpose.trim().to_owned(), None)
        } else {
            let document = documents
                .iter()
                .find(|d| d.id == definition.soul)
                .ok_or_else(|| format!("Missing SOUL.md document: {}.", definition.soul))?;
            if document.body.chars().count() > MAX_SOUL {
                return Err(format!(
                    "SOUL.md {} exceeds {MAX_SOUL} characters; keep it short.",
                    document.id
                ));
            }
            (
                document.body.trim().to_owned(),
                Some((document.id.clone(), document.revision)),
            )
        };
        Ok(Self {
            id: definition.id.clone(),
            name: definition.name.clone(),
            revision: definition.revision,
            purpose,
            soul,
            lenses: definition.lenses.clone(),
            standing: definition.standing.clone(),
            model: definition.model.clone(),
            behaviours: definition.behaviours.clone(),
        })
    }
    /// The persona's first instruction: its purpose, standing and behaviours.
    pub fn prompt(&self) -> String {
        let mut text = format!(
            "Persona {}@{} ({}): {}\nStanding (host-enforced): {}.\n",
            self.id,
            self.revision,
            self.name,
            if self.purpose.is_empty() {
                "no stated purpose"
            } else {
                self.purpose.as_str()
            },
            self.standing.describe()
        );
        let orders = self.behaviours.iter().filter(|b| b.needs_service()).collect::<Vec<_>>();
        if !orders.is_empty() {
            text.push_str("Persona behaviours (standing orders; install each once in a service you own or were granted, with the named tool, then rely on it instead of polling):\n");
            for order in orders {
                text.push_str(&format!("- {}\n", order.describe()));
            }
        }
        text
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Library {
    pub definitions: Vec<Definition>,
    /// The built-in revision this node last installed per persona; a newer
    /// built-in replaces only an unedited copy, as with lenses.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub builtin: BTreeMap<String, u64>,
}
impl Default for Library {
    fn default() -> Self {
        let definitions = builtins();
        let builtin = definitions.iter().map(|d| (d.id.clone(), d.revision)).collect();
        Self { definitions, builtin }
    }
}
/// The shipped definitions.
pub fn builtins() -> Vec<Definition> {
    serde_json::from_str(include_str!("builtin-personas.json")).expect("Built-in persona definitions must be valid")
}
/// The shipped definition `id`: the standing of runs that predate personas.
pub fn builtin(id: &str) -> Option<Definition> {
    builtins().into_iter().find(|d| d.id == id)
}
impl Library {
    /// Offer new built-ins and newer built-in revisions without replacing
    /// local edits; revisions are appended, so captured runs keep theirs.
    pub fn merge_defaults(&mut self) -> bool {
        let mut changed = false;
        for definition in builtins() {
            let current = self.get(&definition.id).map(|d| d.revision);
            let installed = self.builtin.get(&definition.id).copied();
            let install = match current {
                None => self.current().len() < MAX_PERSONAS && self.definitions.len() < MAX_DEFINITIONS,
                Some(current) => {
                    installed == Some(current)
                        && definition.revision > current
                        && self.definitions.len() < MAX_DEFINITIONS
                }
            };
            if install {
                self.builtin.insert(definition.id.clone(), definition.revision);
                self.definitions.push(definition);
                changed = true;
            }
        }
        changed
    }
    pub fn get(&self, id: &str) -> Option<&Definition> {
        self.definitions
            .iter()
            .filter(|d| d.id == id)
            .max_by_key(|d| d.revision)
    }
    pub fn revision(&self, id: &str, revision: u64) -> Option<&Definition> {
        self.definitions.iter().find(|d| d.id == id && d.revision == revision)
    }
    pub fn current(&self) -> Vec<&Definition> {
        let ids: BTreeSet<_> = self.definitions.iter().map(|d| d.id.as_str()).collect();
        ids.into_iter().filter_map(|id| self.get(id)).collect()
    }
    /// Earlier revisions of `id`, newest first.
    pub fn history(&self, id: &str) -> Vec<&Definition> {
        let mut revisions = self.definitions.iter().filter(|d| d.id == id).collect::<Vec<_>>();
        revisions.sort_by_key(|d| std::cmp::Reverse(d.revision));
        revisions
    }
    /// Structure only; the server checks genes, documents and models.
    pub fn validate(&self) -> Result<(), String> {
        if self.current().len() > MAX_PERSONAS || self.definitions.len() > MAX_DEFINITIONS {
            return Err("The persona library is full.".into());
        }
        let mut versions = BTreeSet::new();
        for d in &self.definitions {
            if !d.valid() || !versions.insert((&d.id, d.revision)) {
                return Err(format!(
                    "Invalid persona definition {} or duplicate revision. Node visibility and lead steering are for read-only observers; acting personas steer their team or no one; advisors take no schedule; read-only personas start no routines.",
                    d.id
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "persona/tests.rs"]
mod tests;
