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
    /// Admitted tool packages this lens opts a run into: `name` (latest
    /// admitted when captured) or `name@version` (pinned). Context, not a grant.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
}
/// One admitted package version offered to resolution, like `DocumentInput`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageInput {
    pub name: String,
    pub version: u64,
    pub commit: String,
    pub description: String,
}
/// Split a lens package reference into its name and optional pinned version.
pub fn package_reference(reference: &str) -> Option<(&str, Option<u64>)> {
    let (name, version) = match reference.split_once('@') {
        Some((name, version)) => (name, Some(version.parse::<u64>().ok().filter(|v| *v > 0)?)),
        None => (reference, None),
    };
    valid_id(name).then_some((name, version))
}
#[derive(Clone, Copy)]
struct Inputs<'a> {
    documents: &'a [DocumentInput],
    /// `None` checks package reference syntax only.
    packages: Option<&'a [PackageInput]>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<PackageInput>,
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
    /// Resolve without admitted packages; a lens that names one fails here.
    pub fn resolve(
        &self,
        roots: &[String],
        documents: &[DocumentInput],
    ) -> Result<Resolved, String> {
        self.resolve_with(roots, documents, &[])
    }
    /// Resolve against explicit document and admitted package snapshots.
    pub fn resolve_with(
        &self,
        roots: &[String],
        documents: &[DocumentInput],
        packages: &[PackageInput],
    ) -> Result<Resolved, String> {
        self.resolve_inputs(roots, Inputs { documents, packages: Some(packages) })
    }
    fn resolve_inputs(&self, roots: &[String], inputs: Inputs<'_>) -> Result<Resolved, String> {
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
            self.visit(root, inputs, &mut resolved, &mut done, &mut visiting)?;
        }
        resolved.tools = TOOLS
            .iter()
            .filter(|t| resolved.tools.iter().any(|s| s == t.name))
            .map(|t| t.name.into())
            .collect();
        resolved.documents.sort_by(|a, b| a.id.cmp(&b.id));
        resolved.packages.sort_by(|a, b| a.name.cmp(&b.name));
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
        inputs: Inputs<'_>,
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
            self.visit(child, inputs, out, done, visiting)?;
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
            let input = inputs
                .documents
                .iter()
                .find(|i| &i.id == doc)
                .ok_or_else(|| format!("Missing knowledge document: {doc}."))?;
            if !out.documents.iter().any(|i| i.id == *doc) {
                out.documents.push(input.clone());
            }
        }
        for reference in &d.packages {
            let (name, pinned) = package_reference(reference)
                .ok_or_else(|| format!("Invalid tool package reference: {reference}."))?;
            // Library validation checks references only; admission is a resolution input.
            let Some(admitted) = inputs.packages else { continue };
            let input = admitted
                .iter()
                .filter(|p| p.name == name && pinned.is_none_or(|v| v == p.version))
                .max_by_key(|p| p.version)
                .ok_or_else(|| format!("Tool package not admitted: {reference}."))?;
            match out.packages.iter().find(|p| p.name == name) {
                Some(existing) if existing.version != input.version => {
                    return Err(format!("Selected lenses name different versions of tool package {name}."));
                }
                Some(_) => {}
                None => out.packages.push(input.clone()),
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
                || d.packages.len() > MAX_REFERENCES
                || !versions.insert((&d.id, d.revision))
            {
                return Err("Invalid lens definition or duplicate revision.".into());
            }
        }
        // Revoking a package must not block edits to unrelated lenses, so
        // package admission is checked when a context is captured.
        for d in self.current() {
            self.resolve_inputs(std::slice::from_ref(&d.id), Inputs { documents, packages: None })?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
