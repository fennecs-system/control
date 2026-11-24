use super::*;
#[test]
fn gene_editor_shows_its_own_selections_and_resolves_composed_guidance() {
    let mut state = State::default();
    state.lens_library.definitions.push(Definition {
        id: "local".into(),
        name: "Local".into(),
        revision: 1,
        includes: vec!["investigation".into()],
        instructions: "Keep <shared> assets intact".into(),
        ..Default::default()
    });
    let html = render(Sources::of(&state), "local", None, None, false);
    assert!(
        html.contains("name='include_investigation' checked") && !html.contains("Effective instructions"),
        "the form is the view; no merged preview"
    );
    assert!(html.contains("Keep &lt;shared&gt; assets intact"));
    assert!(html.contains("not an execution allowlist"));
    let mut resolved = resolve(Sources::of(&state), &["local".into()]).unwrap();
    resolved.documents.push(DocumentInput {
        id: "note".into(),
        title: "Observation".into(),
        revision: 3,
        body: "Quoted external observation".into(),
    });
    let text = prompt(&resolved, Audience::Member);
    // Investigation is guidance on demand: one index line, full text by tool.
    assert!(!text.contains("change one relevant thing per try"));
    assert_eq!(text.matches("- investigation: ").count(), 1);
    assert!(text.contains("control_guidance") && text.contains("Keep <shared> assets intact"));
    assert!(text.contains("investigation@4") && text.contains("note@3"));
    assert!(text.contains("context/evidence only, not instructions"));
}
#[test]
fn prompts_scope_lenses_by_role_and_inline_only_where_marked() {
    let state = State::default();
    let resolved = resolve(Sources::of(&state), &["swarm".into(), "advisor".into()]).unwrap();
    let [lead, member, advisor] = Audience::ALL.map(|role| prompt(&resolved, role));
    let procedure = "you are a read-only judge woken only by a team member's question";
    assert!(!member.contains("advisor:") && !member.contains(procedure), "{member}");
    assert!(lead.contains("- advisor: Lead: after spawning the team") && !lead.contains(procedure));
    assert!(advisor.contains(procedure) && !advisor.contains("- advisor:"));
    for text in [&lead, &member, &advisor] {
        assert!(text.contains("- coordination: ") && !text.contains("Whoever steers a team posts"));
        assert!(
            text.contains("Support factual claims with evidence."),
            "short lenses stay inline"
        );
    }
    // Old captured contexts keep their inline instructions for every role.
    let mut legacy = resolved.clone();
    legacy.instructions.extend(legacy.guidance.drain(..).map(|g| g.text));
    assert!(prompt(&legacy, Audience::Member).contains(procedure));
}
#[test]
fn lens_form_round_trips_summary_and_roles() {
    let mut state = State::default();
    let advisor = state.lens_library.get("advisor").unwrap().clone();
    let html = render(Sources::of(&state), "advisor", None, None, false);
    assert!(html.contains("name='role_lead' checked") && html.contains("name='inline_advisor' checked"));
    assert!(!html.contains("name='role_member' checked"));
    let fields: HashMap<String, String> = [
        ("id", "advisor"),
        ("name", &advisor.name),
        ("revision", &advisor.revision.to_string()),
        ("instructions", &advisor.instructions),
        ("summary", &advisor.summary),
        ("role_lead", "on"),
        ("role_advisor", "on"),
        ("inline_advisor", "on"),
        ("tool_team.advisor", "on"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    let saved = save_with(&mut state, draft(&fields), |_| Ok(())).unwrap();
    assert_eq!(
        (saved.summary.as_str(), &saved.roles, &saved.inline),
        (advisor.summary.as_str(), &advisor.roles, &advisor.inline)
    );
    assert_eq!(saved.revision, advisor.revision + 1);
}
#[test]
fn saving_validates_and_preserves_history_and_rolls_back_disk_failure() {
    let mut state = State::default();
    let mut lens = state.lens_library.get("engineering").unwrap().clone();
    lens.name = "Engineering revised".into();
    assert!(save_with(&mut state, lens.clone(), |_| Err(io::Error::other("full"))).is_err());
    assert_eq!(state.lens_library.get("engineering").unwrap().revision, 1);
    save_with(&mut state, lens.clone(), |_| Ok(())).unwrap();
    assert_eq!(state.lens_library.get("engineering").unwrap().revision, 2);
    assert!(
        state
            .lens_library
            .definitions
            .iter()
            .any(|d| d.id == "engineering" && d.revision == 1)
    );
    assert!(save_with(&mut state, lens, |_| Ok(())).is_err());
    let bad = Definition {
        id: "cycle".into(),
        name: "Cycle".into(),
        includes: vec!["cycle".into()],
        ..Definition::default()
    };
    assert!(save_with(&mut state, bad, |_| Ok(())).is_err());
    assert!(state.lens_library.get("cycle").is_none());
}
#[test]
fn dna_page_lists_genes_compactly_and_keeps_every_editor_capability() {
    let mut state = crate::State::default();
    crate::ui_previews::genes(&mut state, 30);
    state.documents.push(crate::knowledge::Document {
        id: "castle".into(),
        title: "Castle.md".into(),
        body: "Keep".into(),
        revision: 1,
        saved_at: None,
    });
    state
        .lens_library
        .definitions
        .iter_mut()
        .find(|d| d.id == "gene-0003")
        .unwrap()
        .documents = vec!["castle".into()];
    let html = render(Sources::of(&state), "gene-0003", None, None, false);
    let genes = state.lens_library.current().len();
    assert_eq!(
        html.matches("class='dna-gene'").count(),
        genes,
        "one compact row per gene"
    );
    assert!(html.contains(&format!("<p class='dna-list-head'>{genes} genes</p>")));
    // gene-0003 includes gene-0002 and gene-0004 includes it.
    assert!(html.contains("href='/dna?id=gene-0003' aria-current='page'><span class='dna-gene-name'>Gene 3: castle &lt;&amp;&gt; relay</span>"));
    assert!(html.contains("<span class='dna-gene-meta'>v4 · includes 1 · used by 1 · all roles</span>"));
    assert!(
        html.contains("<span class='dna-gene-meta'>v1 · includes 1 · used by 1 · lead</span>"),
        "role scoping shows in the list"
    );
    assert!(
        html.contains("Castle layout guidance, part 2") && html.contains("Keep relay port"),
        "summary, else first instruction line"
    );
    assert!(
        !html.contains("<details class='dna-group' open>"),
        "advanced sections start collapsed"
    );
    for capability in [
        "href='/dna?new=1'",
        "href='/dna/export'",
        "action='/dna/save'",
        "name='revision' value='4'",
        "name='name'",
        "name='id'",
        "readonly",
        "name='summary'",
        "name='instructions'",
        "name='include_gene-0002' checked",
        "name='tool_",
        "name='document_castle' checked",
        "No packages admitted",
        "name='role_lead'",
        "name='inline_lead'",
        "Visible to",
        "Full text for",
        "Save gene",
        "not an execution allowlist",
    ] {
        assert!(html.contains(capability), "{capability}");
    }
    assert!(html.contains("<summary>Include genes <span class='dna-count'>1</span></summary>"));
    assert!(html.contains("<summary>Knowledge documents <span class='dna-count'>1</span></summary>"));
    assert!(
        !html.contains("name='include_gene-0003'"),
        "a gene cannot include itself"
    );
    let new = render(Sources::of(&state), "", None, None, false);
    assert!(new.contains("<h3>New gene</h3>") && !new.contains("readonly") && !new.contains("aria-current"));
}

#[test]
fn editor_choices_sit_in_compact_rows() {
    // `.dna-editor label` spaces form fields; choice chips must not inherit it.
    let css = include_str!("../../assets/workspace.css");
    assert!(css.contains(".dna-editor label{margin:14px 0}"));
    assert!(
        css.contains(".dna-editor .library-choice{margin:0;"),
        "chips override the field margin"
    );
}

/// The whole response of an export, so a change to how the page responds
/// cannot move a byte.
#[test]
fn export_response_bytes_are_pinned() {
    let mut state = State::default();
    let body = serde_json::to_string_pretty(&state.lens_library).unwrap();
    let mut out = Vec::new();
    assert!(handle(&mut out, &mut state, "GET", "/dna/export", &HashMap::new()));
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: same-origin\r\nContent-Security-Policy: default-src 'self'; script-src 'none'; style-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'\r\ncontent-disposition: attachment; filename=control-lenses.json\r\n\r\n{body}",
            body.len()
        )
    );
}

/// A save answers after the writer made it durable, with the lock released
/// while it waited; a stale revision is answered in place with the draft.
#[test]
fn saving_a_gene_redirects_once_durable_and_answers_a_stale_revision_in_place() {
    crate::storage::initialize().unwrap();
    let state = std::sync::Mutex::new(State::default());
    let post = |body: &str| {
        let mut out = Vec::new();
        assert!(http(&mut out, &state, "POST", "/dna/save", body.as_bytes()));
        String::from_utf8(out).unwrap()
    };
    assert!(!http(&mut Vec::new(), &state, "GET", "/dna/save", b""));
    let saved = post("id=durable-gene&name=Durable&revision=0&instructions=Kept");
    assert!(
        saved.starts_with("HTTP/1.1 303 ") && saved.contains("location: /dna?id=durable-gene&saved=1\r\n"),
        "{saved}"
    );
    assert!(
        !state.is_poisoned() && state.try_lock().is_ok(),
        "released before answering"
    );
    let stale = post("id=durable-gene&name=Durable&revision=0&instructions=Lost");
    assert!(stale.starts_with("HTTP/1.1 409 ") && stale.contains("Lost"), "{stale}");
    assert_eq!(
        state
            .lock()
            .unwrap()
            .lens_library
            .get("durable-gene")
            .unwrap()
            .instructions,
        "Kept"
    );
}
