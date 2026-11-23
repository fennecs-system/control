
use super::*;
fn fields(id: &str, title: &str, body: &str, rev: u64) -> HashMap<String, String> {
    [
        ("id", id.into()),
        ("title", title.into()),
        ("body", body.into()),
        ("revision", rev.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect()
}
#[test]
fn legacy_draft_is_preserved_and_migration_is_idempotent() {
    let mut s = State {
        note: "  Original draft\n[[other]]\n".into(),
        saved: true,
        ..Default::default()
    };
    migrate(&mut s);
    migrate(&mut s);
    assert_eq!(s.documents.len(), 1);
    assert_eq!(s.documents[0].body, s.note);
    assert_eq!(s.documents[0].revision, 1);
    assert!(s.documents[0].saved_at.is_none());
    let bytes = serde_json::to_vec(&s).unwrap();
    let loaded: State = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(loaded.documents[0].body, s.note);
}
#[test]
fn saves_are_durable_or_rolled_back_and_detect_stale_edits() {
    let mut s = State::default();
    let f = fields("", "One", "first", 0);
    assert!(save_with(&mut s, &f, |_| Err(io::Error::other("disk full"))).is_err());
    assert!(s.documents.is_empty());
    let id = save_with(&mut s, &f, |_| Ok(())).unwrap();
    assert!(
        save_with(&mut s, &fields(&id, "One", "changed", 1), |_| Err(
            io::Error::other("disk full")
        ))
        .is_err()
    );
    assert_eq!(s.documents[0].body, "first");
    save_with(&mut s, &fields(&id, "One", "saved", 1), |_| Ok(())).unwrap();
    assert!(save_with(&mut s, &fields(&id, "One", "stale", 1), |_| Ok(())).is_err());
    assert_eq!(s.documents[0].body, "saved");
    assert_eq!(s.documents[0].revision, 2);
    assert!(save_with(&mut s, &fields("", "one.md", "duplicate", 0), |_| Ok(())).is_err());
}
#[test]
fn graph_uses_real_links_and_escapes_notes() {
    let mut s = State::default();
    let a = save_with(
        &mut s,
        &fields("", "One", "[[Two|a label]] [[missing]] <script>", 0),
        |_| Ok(()),
    )
    .unwrap();
    save_with(&mut s, &fields("", "Two", "", 0), |_| Ok(())).unwrap();
    assert_eq!(edges(&s.documents), BTreeSet::from([(0, 1)]));
    let html = render(&s, &a, Some(1));
    assert!(html.contains("Unresolved · missing"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(html.contains("✓ Saved One.md"));
    assert!(render(&s, "graph", None).contains("<line"));
    if let Ok(directory) = std::env::var("CONTROL_KNOWLEDGE_PREVIEW") {
        std::fs::create_dir_all(&directory).unwrap();
        for (file, content) in [
            ("documents.html", render(&s, &a, Some(1))),
            ("graph.html", render(&s, "graph", None)),
        ] {
            let page = crate::render(&s, "knowledge-page", "research", &content)
                .replace(
                    "<link rel=\"stylesheet\" href=\"/style.css\">",
                    &format!("<style>{}</style>", include_str!("../../assets/style.css")),
                )
                .replace(
                    "<link rel='stylesheet' href='/collective.css'>",
                    &format!(
                        "<style>{}</style>",
                        include_str!("../../assets/collective.css")
                    ),
                )
                .replace(
                    "<link rel='stylesheet' href='/runtime.css'>",
                    &format!(
                        "<style>{}</style>",
                        include_str!("../../assets/runtime.css")
                    ),
                );
            std::fs::write(std::path::Path::new(&directory).join(file), page).unwrap();
        }
    }
}

/// The whole response of a refused save, so a change to how the page
/// responds cannot move a byte.
#[test]
fn refused_save_response_bytes_are_pinned() {
    let mut state = State::default();
    state.documents.retain(|d| d.id != "scratchpad");
    let mut out = Vec::new();
    assert!(handle(&mut out, &mut state, "POST", "/save", &HashMap::new(), "", b""));
    assert_eq!(String::from_utf8(out).unwrap(), "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 17\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: same-origin\r\nContent-Security-Policy: default-src 'self'; script-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'\r\n\r\nNo draft to save.");
}
