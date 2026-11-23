use super::*;
fn form(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}
#[test]
fn knowledge_requires_evidence_and_rejects_duplicates() {
    let mut c = Collective::default();
    c.action("/propose", &form(&[("claim", "A fact")]));
    assert!(c.proposals.is_empty());
    let f = form(&[
        ("claim", "A fact"),
        ("evidence", "artifact:1"),
        ("scope", "test"),
        ("validity", "revision changes"),
    ]);
    c.action("/propose", &f);
    c.action("/propose", &f);
    assert_eq!(c.proposals.len(), 1);
    c.action("/review", &form(&[("id", "0"), ("decision", "accepted")]));
    c.action("/review", &form(&[("id", "0"), ("decision", "rejected")]));
    assert_eq!(c.proposals[0].status, ProposalStatus::Accepted);
}
#[test]
fn requests_need_a_valid_route_and_decisions_are_terminal() {
    let mut c = Collective::default();
    let mut f = form(&[
        ("capability", "network.read"),
        ("resource", "example.org"),
        ("purpose", "Read docs"),
        ("authority", "auto"),
    ]);
    c.action("/request", &f);
    assert!(c.requests.is_empty());
    f.insert("authority".into(), "human".into());
    c.action("/request", &f);
    c.action("/decide", &form(&[("id", "0"), ("decision", "approved")]));
    c.action("/decide", &form(&[("id", "0"), ("decision", "denied")]));
    assert_eq!(c.requests[0].status, RequestStatus::Approved);
}
#[test]
fn rooms_escape_content_and_separate_channels() {
    let mut c = Collective::default();
    c.action(
        "/room",
        &form(&[("channel", "research"), ("message", "<script>oops</script>")]),
    );
    let state = crate::State {
        collective: c,
        ..Default::default()
    };
    assert!(!crate::rooms::render(&state, "general").contains("oops"));
    let page = crate::rooms::render(&state, "research");
    assert!(page.contains("&lt;script&gt;"));
    assert!(!page.contains("<script>"));
}
#[test]
fn saved_proposals_and_requests_load_and_save_back_byte_for_byte() {
    // The saved collective shape, in field order: empty claims and a zero
    // claim sequence are omitted, and a proposal without a source has none.
    let saved = |proposal: &str, request: &str| {
        format!(
            r#"{{"posts":[],"room_watches":[],"proposals":[{{"claim":"A fact","evidence":"artifact:1","scope":"test","validity":"revision changes","status":"{proposal}"}}],"requests":[{{"capability":"network.read","resource":"example.org","purpose":"Read docs","authority":"human","status":"{request}"}}],"notice":""}}"#
        )
    };
    let unknown = ["held-by-an-older-build", ""];
    let proposals = ProposalStatus::KNOWN.iter().map(ProposalStatus::as_str).chain(unknown);
    let requests = RequestStatus::KNOWN.iter().map(RequestStatus::as_str).chain(unknown);
    for (proposal, request) in proposals.zip(requests) {
        let text = saved(proposal, request);
        let collective: Collective = serde_json::from_str(&text).unwrap();
        assert_eq!(serde_json::to_string(&collective).unwrap(), text);
    }
    assert_eq!(
        ProposalStatus::KNOWN
            .iter()
            .map(ProposalStatus::as_str)
            .collect::<Vec<_>>(),
        ["pending", "accepted", "rejected"]
    );
    assert_eq!(
        RequestStatus::KNOWN
            .iter()
            .map(RequestStatus::as_str)
            .collect::<Vec<_>>(),
        ["pending", "approved", "denied"]
    );
    let unknown: Collective = serde_json::from_str(&saved("held-by-an-older-build", "")).unwrap();
    assert_eq!(
        unknown.proposals[0].status,
        ProposalStatus::Unrecognised("held-by-an-older-build".into())
    );
    assert_eq!(unknown.requests[0].status, RequestStatus::Unrecognised(String::new()));
}
#[test]
fn review_status_predicates_keep_their_sets() {
    let set = |test: fn(&ProposalStatus) -> bool| {
        ProposalStatus::KNOWN
            .iter()
            .filter(|s| test(s))
            .map(ProposalStatus::as_str)
            .collect::<Vec<_>>()
    };
    assert_eq!(set(ProposalStatus::is_decision), ["accepted", "rejected"]);
    assert_eq!(set(ProposalStatus::is_standing), ["pending", "accepted"]);
    let decisions = RequestStatus::KNOWN
        .iter()
        .filter(|s| s.is_decision())
        .map(RequestStatus::as_str);
    assert_eq!(decisions.collect::<Vec<_>>(), ["approved", "denied"]);
    // Unknown saved text is standing (it blocks a duplicate claim, as before)
    // and is no decision a form can submit.
    let unknown = ProposalStatus::parse("held-by-an-older-build");
    assert!(unknown.is_standing() && !unknown.is_decision());
    assert!(!RequestStatus::parse("held-by-an-older-build").is_decision());
}
#[test]
fn review_tags_show_the_saved_status_text() {
    let unknown = "held-by-an-older-build";
    let proposals = ProposalStatus::KNOWN
        .iter()
        .map(ProposalStatus::as_str)
        .chain([unknown]);
    let requests = RequestStatus::KNOWN.iter().map(RequestStatus::as_str).chain([unknown]);
    for (proposal, request) in proposals.zip(requests) {
        let text = format!(
            r#"{{"proposals":[{{"claim":"c","evidence":"e","scope":"s","validity":"v","status":"{proposal}"}}],"requests":[{{"capability":"tool.access","resource":"r","purpose":"p","authority":"human","status":"{request}"}}]}}"#
        );
        let c: Collective = serde_json::from_str(&text).unwrap();
        let review = c.render("knowledge-review", "").unwrap();
        assert!(
            review.contains(&format!("<span class='tag'>{proposal}</span>")),
            "{review}"
        );
        assert_eq!(review.contains("action='/review'"), proposal == "pending");
        let authority = c.render("authority", "").unwrap();
        assert!(
            authority.contains(&format!("<span class='tag'>{request}</span>")),
            "{authority}"
        );
        assert_eq!(authority.contains("action='/decide'"), request == "pending");
    }
}
