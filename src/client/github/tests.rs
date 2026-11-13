use super::*;
#[test]
fn terminal_queries_are_bounded_and_cannot_name_repositories_or_urls() {
    assert_eq!(
        Query::parse("170").unwrap(),
        Query {
            pull_request: 170,
            section: Section::Overview,
            page: 1
        }
    );
    assert_eq!(Query::parse("170 checks 2").unwrap().page, 2);
    for text in [
        "",
        "0",
        "170 overview 2",
        "170 checks 0",
        "170 files 1001",
        "170 merge",
        "owner/repo/170",
        "https://github.com/a/b/pull/170",
        "170 checks 1 extra",
    ] {
        assert!(Query::parse(text).is_err(), "{text}");
    }
    for value in [
        serde_json::json!({"pull_request":170,"url":"https://example.com"}),
        serde_json::json!({"pull_request":170,"section":"merge"}),
    ] {
        assert!(serde_json::from_value::<Query>(value).is_err());
    }
}
