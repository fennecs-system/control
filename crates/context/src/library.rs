//! Declarative context functions evaluated over explicit inputs; no resource lookup or IO.
use crate::TOOLS;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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
}
impl Default for Library {
    fn default() -> Self {
        Self {
            definitions: serde_json::from_str(include_str!("builtin-lenses.json"))
                .expect("Built-in lens definitions must be valid"),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
        if roots.len() > 128 {
            return Err("Select at most 128 lenses.".into());
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
        if self.current().len() > 128 || self.definitions.len() > 4096 {
            return Err("The local lens library is full.".into());
        }
        let mut versions = BTreeSet::new();
        for d in &self.definitions {
            if !valid_id(&d.id)
                || d.name.trim().is_empty()
                || d.name.chars().count() > 80
                || d.revision == 0
                || d.instructions.chars().count() > 4000
                || d.includes.len() > 128
                || d.documents.len() > 128
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
mod tests {
    use super::*;
    #[test]
    fn presets_resolve_nested_lenses_once_and_are_pure() {
        let library = Library::default();
        library.validate(&[]).unwrap();
        let a = library
            .resolve(&["engineering".into(), "research".into()], &[])
            .unwrap();
        let b = library
            .resolve(
                &["research".into(), "engineering".into(), "research".into()],
                &[],
            )
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.tools.len(), 5);
        assert_eq!(a.lenses.len(), 8);
    }
    #[test]
    fn cycles_missing_references_and_unknown_tools_fail_closed() {
        let mut library = Library::default();
        let mut d = library.get("engineering").unwrap().clone();
        d.revision = 2;
        d.includes.push("engineering".into());
        library.definitions.push(d);
        assert!(library.validate(&[]).is_err());
        assert!(Library::default()
            .resolve(&["missing".into()], &[])
            .is_err());
        let mut library = Library::default();
        library.definitions[0]
            .tools
            .push("arbitrary.execute".into());
        assert!(library.validate(&[]).is_err());
    }
    #[test]
    fn document_inputs_are_explicit_and_snapshots_survive_definition_changes() {
        let mut library = Library::default();
        let mut d = library.get("architecture-notes").unwrap().clone();
        d.revision = 2;
        d.documents.push("note-1".into());
        library.definitions.push(d);
        assert!(library.resolve(&["engineering".into()], &[]).is_err());
        let inputs = vec![DocumentInput {
            id: "note-1".into(),
            title: "Architecture.md".into(),
            revision: 3,
            body: "A fixed fact".into(),
        }];
        let snapshot = library.resolve(&["engineering".into()], &inputs).unwrap();
        assert_eq!(snapshot.documents[0].revision, 3);
        let mut d = library.get("architecture-notes").unwrap().clone();
        d.revision = 3;
        d.documents.clear();
        library.definitions.push(d);
        assert!(library
            .resolve(&["engineering".into()], &inputs)
            .unwrap()
            .documents
            .is_empty());
        assert_eq!(snapshot.documents[0].body, "A fixed fact");
    }
}
