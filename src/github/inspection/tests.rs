use super::*;
use std::cell::RefCell;
const HEAD: &str = "1234567890123456789012345678901234567890";
struct Mock {
    pull: Value,
    page: Value,
    paths: RefCell<Vec<String>>,
}
impl Mock {
    fn new(page: Value) -> Self {
        Self {
            pull: json!({"number":17,"head":{"sha":HEAD,"repo":{"full_name":"example/project"}},"base":{"repo":{"full_name":"example/project"}},"title":"Review this change","body":"Untrusted evidence","user":{"login":"contributor"}}),
            page,
            paths: RefCell::new(Vec::new()),
        }
    }
}
impl Api for Mock {
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> io::Result<Value> {
        assert_eq!(method, "GET");
        assert!(body.is_none());
        assert!(path.starts_with("/repos/example/project/"));
        self.paths.borrow_mut().push(path.into());
        Ok(if path == "/repos/example/project/pulls/17" {
            self.pull.clone()
        } else {
            self.page.clone()
        })
    }
}
fn query(section: Section) -> Query {
    Query {
        pull_request: 17,
        section,
        page: 1,
    }
}
#[test]
fn reads_use_only_the_bound_repository_and_capture_the_actual_pr_head() {
    for (section, suffix, page) in [
        (Section::Files, "pulls/17/files".into(), json!([])),
        (Section::Comments, "issues/17/comments".into(), json!([])),
        (Section::Reviews, "pulls/17/reviews".into(), json!([])),
        (
            Section::ReviewComments,
            "pulls/17/comments".into(),
            json!([]),
        ),
        (
            Section::Checks,
            format!("commits/{HEAD}/check-runs"),
            json!({"check_runs":[]}),
        ),
        (
            Section::Statuses,
            format!("commits/{HEAD}/statuses"),
            json!([]),
        ),
    ] {
        let api = Mock::new(page);
        let value = inspect(&api, "example/project", &query(section)).unwrap();
        assert_eq!(value["head_sha"], HEAD);
        assert_eq!(
            api.paths.borrow()[1],
            format!("/repos/example/project/{suffix}?per_page=5&page=1")
        );
        assert!(
            value.get("success").is_none(),
            "empty checks are not a passing build"
        );
        if matches!(section, Section::Checks | Section::Statuses) {
            assert!(
                value["notice"]
                    .as_str()
                    .unwrap()
                    .contains("do not establish CI success")
            );
        }
    }
    let mut api = Mock::new(json!({"check_runs":[]}));
    api.pull["head"]["repo"]["full_name"] = json!("someone/fork");
    assert!(inspect(&api, "example/project", &query(Section::Checks)).is_err());
    assert_eq!(
        api.paths.borrow().len(),
        1,
        "never follow a fork URL with broker credentials"
    );
    assert!(inspect(&api, "example/project", &query(Section::Overview)).is_ok());
}
#[test]
fn missing_wrong_or_malformed_pr_identity_and_pages_fail_visibly() {
    let mut api = Mock::new(Value::Null);
    for pull in [
        Value::Null,
        json!({"number":18}),
        json!({"number":17,"base":{"repo":{"full_name":"other/repository"}}}),
    ] {
        api.pull = pull;
        assert!(inspect(&api, "example/project", &query(Section::Files)).is_err());
    }
    let api = Mock::new(json!([]));
    assert!(inspect(&api, "example/project/../other", &query(Section::Files)).is_err());
    assert!(api.paths.borrow().is_empty());
    let api = Mock::new(Value::Null);
    assert!(inspect(&api, "example/project", &query(Section::Files)).is_err());
    let api = Mock::new(json!([{}, {}, {}, {}, {}, {}]));
    assert!(inspect(&api, "example/project", &query(Section::Files)).is_err());
}
#[test]
fn excerpts_are_utf8_bounded_with_pagination_and_no_unknown_fields() {
    let api=Mock::new(json!((0..PAGE_SIZE).map(|id|json!({"id":id,"body":"猫".repeat(2000),"user":{"login":"author"},"unexpected_private_field":"do not forward"})).collect::<Vec<_>>()));
    let mut input = query(Section::Comments);
    input.page = 2;
    let value = inspect(&api, "example/project", &input).unwrap();
    assert_eq!(value["next_page"], 3);
    assert_eq!(value["items"][0]["body"].as_str().unwrap().len(), 1023);
    assert_eq!(value["items"][0]["truncated"], true);
    assert_eq!(value["items"][0]["author"], "author");
    assert!(!value.to_string().contains("do not forward"));
    assert!(value.to_string().len() < OUTPUT_BYTES);
    input.page = 1000;
    let value = inspect(&api, "example/project", &input).unwrap();
    assert!(value["next_page"].is_null());
    assert_eq!(value["pagination_limit_reached"], true);
}
#[test]
fn ordinary_stale_judge_and_changed_owner_attempts_cannot_inspect() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "/improve Inspect a PR", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    state.runtime.runs[0].attempts.push(Attempt {
        workspace_id: Some("current-generation".into()),
        ..Default::default()
    });
    let run = state.runtime.runs[0].id.clone();
    let caller = Caller::Attempt {
        run: &run,
        generation: "current-generation",
    };
    assert!(authorize(&state, caller).is_ok());
    assert!(authorize(&state, Caller::Chat(&chat)).is_ok());
    assert!(
        authorize(
            &state,
            Caller::Attempt {
                run: &run,
                generation: "old-generation"
            }
        )
        .is_err()
    );
    state.runtime.runs[0].action = Action::Task;
    assert!(authorize(&state, caller).is_err());
    state.runtime.runs[0].action = Action::Improve;
    state.runtime.runs[0].attempts[0].review_only = true;
    // is_judge uses the captured persona; do not infer permission from prose.
    state.runtime.runs[0].persona = Some(crate::delegation::Persona {
        role: crate::delegation::Role::Judge,
        root: "fixture".into(),
        depth: 1,
        parent: Some("parent".into()),
    });
    assert!(authorize(&state, caller).is_err());
    state.runtime.runs[0].persona = None;
    state.chats[0].actor = Actor::Unknown;
    assert!(authorize(&state, caller).is_err());
    state.chats[0].actor = Actor::LocalOperator;
    state.runtime.runs[0].status = "completed".into();
    assert!(authorize(&state, caller).is_err());
    let shared = Arc::new(Mutex::new(state));
    assert!(
        execute(&shared, caller, query(Section::Overview)).is_err(),
        "authorization fails before configuration or credentials are read"
    );
}
