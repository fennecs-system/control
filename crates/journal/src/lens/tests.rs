use super::*;
use crate::snapshot::{apply, diff, edits, encode};
use serde_json::json;

/// Version 1 renames `assignee`, wraps it into a list and gives every task
/// `tags`; the step is stored as data in the schema.
fn schemas() -> (Schema, Schema) {
    let step = vec![Lens::In {
        name: "tasks".into(),
        lens: vec![Lens::Map {
            lens: vec![
                Lens::Rename {
                    from: "assignee".into(),
                    to: "assignees".into(),
                },
                Lens::Wrap {
                    name: "assignees".into(),
                },
                Lens::Add {
                    name: "tags".into(),
                    default: json!([]),
                },
            ],
        }],
    }];
    (
        Schema::default(),
        Schema {
            version: 1,
            lenses: vec![step],
        },
    )
}
fn old() -> Value {
    json!({"tasks": [{"title": "a", "assignee": "ada"}, {"title": "b", "assignee": null}]})
}
fn new() -> Value {
    json!({"tasks": [{"title": "a", "assignees": ["ada"], "tags": []}, {"title": "b", "assignees": [], "tags": []}]})
}

#[test]
fn documents_translate_both_ways() {
    let (v0, v1) = schemas();
    let mut doc = old();
    document(&route(&v0, &v1).unwrap(), &mut doc);
    assert_eq!(doc, new());
    // An older reader walks the newer writer's recorded steps backwards.
    document(&route(&v1, &v0).unwrap(), &mut doc);
    assert_eq!(
        doc,
        json!({"tasks": [{"title": "a", "assignee": "ada"}, {"title": "b", "assignee": null}]})
    );
    assert!(
        route(
            &v1,
            &Schema {
                version: 2,
                lenses: vec![vec![]]
            }
        )
        .is_err(),
        "no recorded step to version 2"
    );
}

/// A patch written against one version, translated and applied to the other
/// version's document, gives the translated result of applying it first.
#[test]
fn patches_translate_like_the_documents_they_change() {
    let (v0, v1) = schemas();
    let mut changed = old();
    changed["tasks"][0]["assignee"] = json!("grace");
    changed["tasks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"title": "c", "assignee": "lin"}));
    let patch = diff(&encode(&old()).unwrap(), &encode(&changed).unwrap()).unwrap();
    let forward = route(&v0, &v1).unwrap();
    let mut doc = new();
    apply(
        &mut doc,
        edits(&patch)
            .unwrap()
            .into_iter()
            .filter_map(|e| edit(&forward, e))
            .collect(),
    )
    .unwrap();
    document(&forward, &mut changed);
    assert_eq!(doc, changed);

    // Backwards: a newer writer's change to `tags` has no counterpart and is dropped.
    let mut newer = new();
    newer["tasks"][1]["tags"] = json!(["x"]);
    newer["tasks"][1]["assignees"] = json!(["bo"]);
    let patch = diff(&encode(&new()).unwrap(), &encode(&newer).unwrap()).unwrap();
    let backward = route(&v1, &v0).unwrap();
    let mut doc = old();
    apply(
        &mut doc,
        edits(&patch)
            .unwrap()
            .into_iter()
            .filter_map(|e| edit(&backward, e))
            .collect(),
    )
    .unwrap();
    assert_eq!(doc["tasks"][1], json!({"title": "b", "assignee": "bo"}));
}

#[test]
fn every_lens_reverses_to_itself() {
    let (_, v1) = schemas();
    for lens in v1.lenses.concat() {
        assert_eq!(lens.reverse().reverse(), lens);
    }
    let stored = serde_json::to_value(&v1).unwrap();
    assert_eq!(serde_json::from_value::<Schema>(stored).unwrap(), v1, "lenses are data");
}

/// Moving data out of the document reads forward unchanged and refuses to be
/// read back by a schema that would silently miss it.
#[test]
fn an_external_step_reads_forward_and_refuses_backwards() {
    let v1 = Schema {
        version: 1,
        lenses: vec![vec![]],
    };
    let v2 = Schema {
        version: 2,
        lenses: vec![vec![], vec![Lens::External { name: "runs".into() }]],
    };
    let mut doc = old();
    document(&route(&v1, &v2).unwrap(), &mut doc);
    assert_eq!(doc, old());
    assert!(route(&v2, &v1).is_err());
}
