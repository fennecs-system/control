
use super::*;
fn form(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
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
    assert_eq!(c.proposals[0].status, "accepted");
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
    assert_eq!(c.requests[0].status, "approved");
}
#[test]
fn rooms_escape_content_and_separate_channels() {
    let mut c = Collective::default();
    c.action(
        "/room",
        &form(&[
            ("channel", "research"),
            ("message", "<script>oops</script>"),
        ]),
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
