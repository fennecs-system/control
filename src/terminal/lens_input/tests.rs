use super::*;
use serde_json::json;

fn library(id: &str) -> bool { matches!(id, "alpha" | "beta" | "gamma") }
fn marks(text: &str) -> Option<Vec<(&str, bool)>> {
    Some(highlight(text, library)?.map(|(range, valid)| (&text[range], valid)).collect())
}

#[test]
fn parses_single_space_and_comma_lists() {
    assert_eq!(parse("/use alpha"), Some(vec!["alpha"]));
    assert_eq!(parse("/use alpha beta  gamma"), Some(vec!["alpha", "beta", "gamma"]));
    assert_eq!(parse("/use alpha,beta"), Some(vec!["alpha", "beta"]));
    assert_eq!(parse("/use alpha, beta ,gamma,"), Some(vec!["alpha", "beta", "gamma"]));
    assert_eq!(parse("/use none"), Some(Vec::new()));
    assert_eq!(parse("/use none alpha"), Some(vec!["none", "alpha"]));
    assert_eq!(names("/use , "), Some(Vec::new()));
    assert_eq!(parse("/user alpha"), None);
    assert_eq!(parse("/use"), None);
    assert_eq!(parse("use alpha"), None);
    assert!(is_use("/use ") && !is_use("/use") && !is_use("/users"));
}

#[test]
fn reports_unknown_names_once_in_input_order() {
    let names = parse("/use zeta alpha typo, zeta").unwrap();
    assert_eq!(unknown(&names, library), vec!["zeta", "typo"]);
    assert!(unknown(&parse("/use alpha,beta").unwrap(), library).is_empty());
}

#[test]
fn highlight_marks_each_name_by_library_membership() {
    assert_eq!(marks("/use alpha, typo beta").unwrap(), vec![("alpha", true), ("typo", false), ("beta", true)]);
    assert_eq!(highlight("/use none", library).unwrap().collect::<Vec<_>>(), vec![(5..9, true)]);
    assert_eq!(marks("/use none, ").unwrap(), vec![("none", true)]);
    assert_eq!(marks("/use none alpha").unwrap(), vec![("none", false), ("alpha", true)]);
    assert_eq!(marks("/use ").unwrap(), Vec::new());
    assert!(marks("/lenses").is_none());
}

#[test]
fn highlight_ranges_respect_multibyte_names() {
    let text = "/use é,\u{3000}ß";
    let ranges: Vec<_> = highlight(text, |_| false).unwrap().map(|(range, _)| &text[range]).collect();
    assert_eq!(ranges, vec!["é", "ß"]);
}

#[test]
fn catalog_uses_library_ids_and_degrades_without_one() {
    let catalog = Catalog::from_list(&json!([{"id":"alpha","name":"Alpha"},{"id":"swarm"}]));
    assert_eq!(catalog.known().map(|ids| ids.contains("swarm")), Some(true));
    assert_eq!(catalog.known().map(|ids| ids.contains("Alpha")), Some(false));
    assert!(Catalog::from_list(&json!({"error":"no"})).known().is_none());
    assert!(Catalog::Unloaded.known().is_none());
}
