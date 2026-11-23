//! Self-contained scratch renders of operator pages for `control browser capture`:
//! `CONTROL_PREVIEW_DIR=/abs/dir cargo test ui_previews -- --ignored`.
//! Synthetic state only; nothing reads or writes the operator's node.
use crate::State;

/// Inline every Control stylesheet so the file renders without the server.
pub(crate) fn self_contained(page: &str) -> String {
    let mut page = page.replace(
        "<link rel=\"stylesheet\" href=\"/style.css\">",
        "<link rel='stylesheet' href='/style.css'>",
    );
    for (name, css) in [
        ("style", include_str!("../../../assets/style.css")),
        ("collective", include_str!("../../../assets/collective.css")),
        ("runtime", include_str!("../../../assets/runtime.css")),
        ("workspace", include_str!("../../../assets/workspace.css")),
        ("navigation", include_str!("../../../assets/navigation.css")),
    ] {
        page = page.replace(
            &format!("<link rel='stylesheet' href='/{name}.css'>"),
            &format!("<style>{css}</style>"),
        );
    }
    page
}

/// `count` synthetic genes beside the defaults, each including its
/// predecessor and naming a few tools, roles and a summary.
pub(crate) fn genes(state: &mut State, count: usize) {
    let tools = control_context::TOOLS
        .iter()
        .map(|tool| tool.name.to_owned())
        .collect::<Vec<_>>();
    for index in 0..count {
        state.lens_library.definitions.push(control_context::Definition {
            id: format!("gene-{index:04}"),
            name: format!("Gene {index}: castle <&> relay"),
            revision: 1 + index as u64 % 4,
            includes: if index == 0 {
                vec!["engineering".into()]
            } else {
                vec![format!("gene-{:04}", index - 1)]
            },
            tools: tools.iter().skip(index % 3).step_by(5).cloned().collect(),
            instructions: format!("Keep relay port {} open; record castle decisions.", 25565 + index % 7),
            summary: if index % 2 == 0 {
                format!("Castle layout guidance, part {index}")
            } else {
                String::new()
            },
            roles: if index % 3 == 0 {
                vec![control_context::Audience::Lead]
            } else {
                Vec::new()
            },
            ..Default::default()
        });
    }
}

fn write(directory: &std::path::Path, file: &str, page: &str) {
    std::fs::write(directory.join(file), self_contained(page)).unwrap();
}

#[test]
#[ignore = "writes preview files"]
fn ui_previews() {
    let directory = std::path::PathBuf::from(
        std::env::var("CONTROL_PREVIEW_DIR").expect("CONTROL_PREVIEW_DIR=/absolute/directory"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let mut state = State::default();
    genes(&mut state, 24);
    for _ in 0..3 {
        crate::chat::create(&mut state);
    }
    state
        .runtime
        .enqueue("Preview".into(), crate::runtime::RunKind::Chat, "test");
    let dna = crate::lens_library::render(&state, "gene-0003", None, None, false);
    write(
        &directory,
        "dna.html",
        &crate::render(&state, "lens-page", "none", &dna),
    );
    write(
        &directory,
        "usage.html",
        &crate::render(&state, "monitor", "none", "general"),
    );
    let landing = crate::chat::landing(&state, "none", None);
    write(
        &directory,
        "landing.html",
        &crate::render(&state, "session-list", "none", &landing),
    );
}
