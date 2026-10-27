//! Pure lens selection. Resource permissions remain outside this function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tool {
    pub name: &'static str,
    pub execution: &'static str,
    pub surface: &'static str,
    pub available: bool,
}
pub const TOOLS: [Tool; 5] = [
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
        execution: "remote node",
        surface: "search window",
        available: false,
    },
];
pub fn lens<'a>(category: &str, tools: &'a [Tool]) -> Vec<&'a Tool> {
    tools
        .iter()
        .filter(|tool| match category {
            "engineering" => tool.name != "search.read",
            "research" => tool.name != "repo.read" && tool.name != "repo.patch",
            _ => false,
        })
        .collect()
}
pub fn inspect(category: &str, authorized: &[&str]) -> String {
    let mut text = format!("Lens: {category}\nPure selection over the supplied registry.\n\n");
    for tool in lens(category, &TOOLS) {
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
}
