use super::*;
#[test]
fn write_commands_preserve_text_and_require_a_specific_head_for_merge() {
    let Command::Write(Operation::Comment { body, .. }) =
        Command::parse("17  comment  First line\nSecond line").unwrap()
    else {
        panic!("expected comment")
    };
    assert_eq!(body, "First line\nSecond line");
    let Command::Write(Operation::Merge { method, .. }) =
        Command::parse("17 merge 1234567890123456789012345678901234567890").unwrap()
    else {
        panic!("expected merge")
    };
    assert_eq!(method, MergeMethod::Squash);
    for text in [
        "17 merge",
        "17 merge main",
        "17 merge 1234567890123456789012345678901234567890 bypass",
        "17 comment ",
        "17 title ",
        "0 body text",
    ] {
        assert!(Command::parse(text).is_err(), "{text}");
    }
    for value in [
        serde_json::json!({"action":"merge","pull_request":17,"expected_head_sha":"1234567890123456789012345678901234567890","bypass":true}),
        serde_json::json!({"action":"comment","pull_request":17,"body":"text","repository":"other/repo"}),
        serde_json::json!({"action":"close","pull_request":17}),
    ] {
        assert!(serde_json::from_value::<Operation>(value).is_err());
    }
    assert!(
        Operation::Update {
            pull_request: 17,
            title: None,
            body: None
        }
        .validate()
        .is_err()
    );
    assert!(
        Operation::Comment {
            pull_request: 17,
            body: "猫".repeat(5334)
        }
        .validate()
        .is_err()
    );
}
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
