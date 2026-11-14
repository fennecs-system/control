//! Declarative context functions evaluated over explicit inputs; no resource lookup or IO.
use crate::TOOLS;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Stored definitions, all revisions included.
const MAX_DEFINITIONS: usize = 4096;
/// Distinct current lenses.
const MAX_LENSES: usize = 128;
/// Lenses selected for one resolution.
const MAX_SELECTED: usize = 128;
/// Includes or documents named by one definition.
const MAX_REFERENCES: usize = 128;

#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Definition {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub includes: Vec<String>,
    pub tools: Vec<String>,
    pub documents: Vec<String>,
    pub instructions: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Library {
    pub definitions: Vec<Definition>,
    /// The built-in revision this node last installed per lens. A later
    /// built-in revision is installed only while that one is still current,
    /// so a local edit (a newer local revision) is never replaced.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub builtin: std::collections::BTreeMap<String, u64>,
}
impl Default for Library {
    fn default() -> Self {
        let definitions: Vec<Definition> = serde_json::from_str(include_str!("builtin-lenses.json"))
            .expect("Built-in lens definitions must be valid");
        let builtin = definitions.iter().map(|d| (d.id.clone(), d.revision)).collect();
        Self { definitions, builtin }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentInput {
    pub id: String,
    pub title: String,
    pub revision: u64,
    pub body: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LensVersion {
    pub id: String,
    pub name: String,
    pub revision: u64,
}
#[derive(Clone, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolved {
    pub roots: Vec<String>,
    pub lenses: Vec<LensVersion>,
    pub tools: Vec<String>,
    pub documents: Vec<DocumentInput>,
    pub instructions: Vec<String>,
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
impl Library {
    /// Offer new built-ins and newer built-in revisions to existing nodes
    /// without replacing local edits or selecting anything for a chat. New
    /// revisions are appended, so captured run contexts keep their revision.
    pub fn merge_defaults(&mut self) -> bool {
        // Earlier shipped revisions identify unedited copies on nodes that
        // predate the installed-revision record.
        let shipped: Vec<Definition> = serde_json::from_str(include_str!("builtin-lens-history.json"))
            .expect("Built-in lens history must be valid");
        let mut changed = false;
        let mut lenses = self.current().len();
        for definition in Self::default().definitions {
            let current = self.get(&definition.id).cloned();
            let mut installed = self.builtin.get(&definition.id).copied();
            if let Some(current) = current.as_ref().filter(|c| installed.is_none() && (**c == definition || shipped.contains(c))) {
                self.builtin.insert(definition.id.clone(), current.revision);
                installed = Some(current.revision);
                changed = true;
            }
            match current {
                None if self.definitions.len() < MAX_DEFINITIONS && lenses < MAX_LENSES => lenses += 1,
                Some(current) if installed == Some(current.revision)
                    && definition.revision > current.revision
                    && self.definitions.len() < MAX_DEFINITIONS => {}
                _ => continue,
            }
            self.builtin.insert(definition.id.clone(), definition.revision);
            self.definitions.push(definition);
            changed = true;
        }
        changed
    }

    pub fn get(&self, id: &str) -> Option<&Definition> {
        self.definitions
            .iter()
            .filter(|d| d.id == id)
            .max_by_key(|d| d.revision)
    }
    pub fn current(&self) -> Vec<&Definition> {
        let ids: BTreeSet<_> = self.definitions.iter().map(|d| d.id.as_str()).collect();
        ids.into_iter().filter_map(|id| self.get(id)).collect()
    }
    pub fn resolve(
        &self,
        roots: &[String],
        documents: &[DocumentInput],
    ) -> Result<Resolved, String> {
        let roots: Vec<_> = roots
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if roots.len() > MAX_SELECTED {
            return Err(format!("Select at most {MAX_SELECTED} lenses."));
        }
        let mut resolved = Resolved {
            roots: roots.clone(),
            ..Resolved::default()
        };
        let mut done = BTreeSet::new();
        let mut visiting = BTreeSet::new();
        for root in &roots {
            self.visit(root, documents, &mut resolved, &mut done, &mut visiting)?;
        }
        resolved.tools = TOOLS
            .iter()
            .filter(|t| resolved.tools.iter().any(|s| s == t.name))
            .map(|t| t.name.into())
            .collect();
        resolved.documents.sort_by(|a, b| a.id.cmp(&b.id));
        if resolved
            .documents
            .iter()
            .map(|d| d.body.chars().count())
            .sum::<usize>()
            + resolved
                .instructions
                .iter()
                .map(|s| s.chars().count())
                .sum::<usize>()
            > 48000
        {
            return Err(
                "Selected context exceeds 48,000 characters. Select fewer documents or lenses."
                    .into(),
            );
        }
        Ok(resolved)
    }
    fn visit(
        &self,
        id: &str,
        documents: &[DocumentInput],
        out: &mut Resolved,
        done: &mut BTreeSet<String>,
        visiting: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if done.contains(id) {
            return Ok(());
        }
        if visiting.len() >= 32 || !visiting.insert(id.into()) {
            return Err(format!("Lens cycle or excessive nesting at {id}."));
        }
        let d = self.get(id).ok_or_else(|| format!("Unknown lens: {id}."))?;
        for child in &d.includes {
            self.visit(child, documents, out, done, visiting)?;
        }
        for tool in &d.tools {
            if !TOOLS.iter().any(|t| t.name == tool) {
                return Err(format!("Unknown tool: {tool}."));
            }
            if !out.tools.contains(tool) {
                out.tools.push(tool.clone());
            }
        }
        for doc in &d.documents {
            let input = documents
                .iter()
                .find(|i| &i.id == doc)
                .ok_or_else(|| format!("Missing knowledge document: {doc}."))?;
            if !out.documents.iter().any(|i| i.id == *doc) {
                out.documents.push(input.clone());
            }
        }
        if !d.instructions.trim().is_empty() && !out.instructions.contains(&d.instructions) {
            out.instructions.push(d.instructions.clone());
        }
        out.lenses.push(LensVersion {
            id: d.id.clone(),
            name: d.name.clone(),
            revision: d.revision,
        });
        visiting.remove(id);
        done.insert(id.into());
        Ok(())
    }
    pub fn validate(&self, documents: &[DocumentInput]) -> Result<(), String> {
        if self.current().len() > MAX_LENSES || self.definitions.len() > MAX_DEFINITIONS {
            return Err("The local lens library is full.".into());
        }
        let mut versions = BTreeSet::new();
        for d in &self.definitions {
            if !valid_id(&d.id)
                || d.name.trim().is_empty()
                || d.name.chars().count() > 80
                || d.revision == 0
                || d.instructions.chars().count() > 4000
                || d.includes.len() > MAX_REFERENCES
                || d.documents.len() > MAX_REFERENCES
                || !versions.insert((&d.id, d.revision))
            {
                return Err("Invalid lens definition or duplicate revision.".into());
            }
        }
        for d in self.current() {
            self.resolve(std::slice::from_ref(&d.id), documents)?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
