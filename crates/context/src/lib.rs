//! Pure lens selection. Resource permissions remain outside this function.
mod library;
pub use library::{valid_id, Definition, DocumentInput, LensVersion, Library, Resolved};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tool {
    pub name: &'static str,
    pub execution: &'static str,
    pub surface: &'static str,
    pub available: bool,
}
pub const TOOLS: [Tool; 7] = [
    Tool {
        name: "context.inspect",
        execution: "pure Rust",
        surface: "embedded window / WASM",
        available: true,
    },
    Tool {
        name: "knowledge.propose",
        execution: "local Rust server",
        surface: "review form",
        available: true,
    },
    Tool {
        name: "repo.read",
        execution: "Codex container",
        surface: "artifact viewer",
        available: false,
    },
    Tool {
        name: "repo.patch",
        execution: "Codex container",
        surface: "patch review",
        available: false,
    },
    Tool {
        name: "search.read",
        execution: "host search broker",
        surface: "search window",
        available: true,
    },
    Tool {
        name: "rust.run",
        execution: "offline Nix container",
        surface: "Rust playground",
        available: true,
    },
    Tool {
        name: "browser.capture",
        execution: "offline Nix Firefox",
        surface: "screenshot",
        available: true,
    },
];
pub fn lens<'a>(category: &str, tools: &'a [Tool]) -> Vec<&'a Tool> {
    compose(&[category], tools)
}
pub fn inspect(category: &str, authorized: &[&str]) -> String {
    inspect_composed(&[category], authorized)
}
/// Compose selections by union, retaining registry order and removing shared descriptors.
pub fn compose<'a>(categories: &[&str], tools: &'a [Tool]) -> Vec<&'a Tool> {
    let library = Library::default();
    let roots = categories
        .iter()
        .filter(|id| library.get(id).is_some())
        .map(|id| (*id).into())
        .collect::<Vec<_>>();
    let resolved = library.resolve(&roots, &[]).unwrap_or_default();
    tools
        .iter()
        .filter(|tool| resolved.tools.iter().any(|id| id == tool.name))
        .collect()
}
pub fn inspect_composed(categories: &[&str], authorized: &[&str]) -> String {
    let mut text = format!(
        "Lenses: {}\nPure selection over the supplied registry.\n\n",
        categories.join(" + ")
    );
    for tool in compose(categories, &TOOLS) {
        let status = if !tool.available {
            "not connected"
        } else if authorized.contains(&tool.name) {
            "permitted in this local example"
        } else {
            "not authorized"
        };
        text.push_str(&format!(
            "{}\n  execution: {}\n  presentation: {}\n  status: {}\n\n",
            tool.name, tool.execution, tool.surface, status
        ));
    }
    text.push_str("Selection does not grant authority. No side effects occur here.");
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_is_deterministic_and_does_not_expand_unknown_lenses() {
        assert_eq!(lens("engineering", &TOOLS), lens("engineering", &TOOLS));
        assert!(lens("missing", &TOOLS).is_empty());
        assert!(!lens("research", &TOOLS)
            .iter()
            .any(|t| t.name == "repo.patch"));
    }
    #[test]
    fn selected_tools_still_need_authority() {
        assert!(inspect("engineering", &[]).contains("not authorized"));
        assert!(inspect("engineering", &["context.inspect"])
            .contains("permitted in this local example"));
        assert!(inspect("engineering", &["repo.patch"]).contains("not connected"));
    }
    #[test]
    fn composition_is_deduplicated_and_order_independent() {
        let combined = compose(&["engineering", "research"], &TOOLS);
        assert_eq!(combined.len(), 5);
        assert_eq!(
            combined,
            compose(&["research", "engineering", "research"], &TOOLS)
        );
        assert!(compose(&[], &TOOLS).is_empty());
        assert!(inspect_composed(&["engineering", "research"], &[]).contains("not authorized"));
    }
}
