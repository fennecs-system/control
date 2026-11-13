use super::*;
use control::client::github::MergeMethod;
use std::cell::RefCell;

const HEAD: &str = "1234567890123456789012345678901234567890";
struct Mock {
    pull: Value,
    response: Value,
    calls: RefCell<Vec<(String, String, Option<Value>)>>,
    reject_write: bool,
}
impl Mock {
    fn new(response: Value) -> Self {
        Self {
            pull: json!({"number":17,"head":{"sha":HEAD},"base":{"repo":{"full_name":"example/project"},"ref":"main"},"state":"open","draft":false,"merged":false}),
            response,
            calls: RefCell::new(Vec::new()),
            reject_write: false,
        }
    }
}
impl Api for Mock {
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> io::Result<Value> {
        self.calls
            .borrow_mut()
            .push((method.into(), path.into(), body));
        if method == "GET" {
            return Ok(self.pull.clone());
        }
        if self.reject_write {
            return Err(fail("Simulated lost response or revoked authority"));
        }
        Ok(self.response.clone())
    }
}
fn merge() -> Operation {
    Operation::Merge {
        pull_request: 17,
        expected_head_sha: HEAD.into(),
        method: MergeMethod::Squash,
    }
}
#[test]
fn merge_is_opt_in_binds_head_and_base_and_has_no_bypass_parameters() {
    let api = Mock::new(json!({"merged":true,"sha":HEAD,"unexpected":"not forwarded"}));
    assert!(mutate(&api, "example/project", "main", false, &merge()).is_err());
    assert!(api.calls.borrow().is_empty());
    let result = mutate(&api, "example/project", "main", true, &merge()).unwrap();
    assert_eq!(
        api.calls.borrow()[1],
        (
            "PUT".into(),
            "/repos/example/project/pulls/17/merge".into(),
            Some(json!({"sha":HEAD,"merge_method":"squash"}))
        )
    );
    assert_eq!(result["result"]["merged"], true);
    assert!(!result.to_string().contains("not forwarded"));
    for (field, value) in [
        (
            "head",
            json!({"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}),
        ),
        (
            "base",
            json!({"repo":{"full_name":"example/project"},"ref":"other"}),
        ),
        (
            "base",
            json!({"repo":{"full_name":"other/project"},"ref":"main"}),
        ),
        ("state", json!("closed")),
        ("draft", json!(true)),
        ("draft", Value::Null),
    ] {
        let mut api = Mock::new(Value::Null);
        api.pull[field] = value;
        assert!(
            mutate(&api, "example/project", "main", true, &merge()).is_err(),
            "{field}"
        );
        assert_eq!(api.calls.borrow().len(), 1, "must stop before a write");
    }
}
#[test]
fn explicit_merge_retry_observes_already_merged_without_repeating_the_effect() {
    let mut api = Mock::new(Value::Null);
    api.pull["merged"] = json!(true);
    api.pull["state"] = json!("closed");
    let result = mutate(&api, "example/project", "main", true, &merge()).unwrap();
    assert_eq!(result["already_merged"], true);
    assert_eq!(api.calls.borrow().len(), 1);
    api.pull["head"]["sha"] = json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert!(mutate(&api, "example/project", "main", true, &merge()).is_err());
}
#[test]
fn comment_and_update_send_only_the_requested_fields_and_do_not_retry() {
    let api = Mock::new(
        json!({"id":99,"html_url":"https://github.com/example/project/pull/17#issuecomment-99","body":"do not echo","unexpected":"do not forward"}),
    );
    let comment = Operation::Comment {
        pull_request: 17,
        body: "Review findings\n\nReady for another look.".into(),
    };
    let result = mutate(&api, "example/project", "main", false, &comment).unwrap();
    assert_eq!(
        api.calls.borrow()[1],
        (
            "POST".into(),
            "/repos/example/project/issues/17/comments".into(),
            Some(json!({"body":"Review findings\n\nReady for another look."}))
        )
    );
    assert_eq!(result["result"]["comment"]["id"], 99);
    assert!(!result.to_string().contains("do not"));
    let api = Mock::new(json!({"number":17}));
    let operation = Operation::Update {
        pull_request: 17,
        title: None,
        body: Some(String::new()),
    };
    mutate(&api, "example/project", "main", false, &operation).unwrap();
    assert_eq!(api.calls.borrow()[1].2, Some(json!({"body":""})));
    let mut api = Mock::new(Value::Null);
    api.reject_write = true;
    assert!(mutate(&api, "example/project", "main", false, &comment).is_err());
    assert_eq!(
        api.calls.borrow().len(),
        2,
        "one lookup and one write, no retry"
    );
}
#[test]
fn malformed_or_rejected_write_results_are_not_success() {
    for response in [
        Value::Null,
        json!({"merged":false}),
        json!({"merged":true,"sha":"bad"}),
    ] {
        let api = Mock::new(response);
        assert!(mutate(&api, "example/project", "main", true, &merge()).is_err());
        assert_eq!(api.calls.borrow().len(), 2);
    }
    let operation = Operation::Comment {
        pull_request: 17,
        body: "A comment".into(),
    };
    let api = Mock::new(Value::Null);
    assert!(
        mutate(&api, "example/project", "main", false, &operation)
            .unwrap_err()
            .to_string()
            .contains("outcome is unknown")
    );
    assert_eq!(api.calls.borrow().len(), 2);
}
#[test]
fn unauthorized_mutations_fail_before_config_or_credential_access() {
    let state = Arc::new(Mutex::new(State::default()));
    assert!(execute(&state, Caller::Chat("not-authorized"), merge()).is_err());
    assert!(
        execute(
            &state,
            Caller::Attempt {
                run: "not-active",
                generation: "old"
            },
            merge()
        )
        .is_err()
    );
}

#[test]
fn write_authority_is_lost_when_a_handoff_or_decision_is_staged() {
    use control_context::{
        turn_mailbox::{Disposition, Handoff, Mailbox},
        working_state::Payload,
    };
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "/improve Inspect a change", "test").unwrap();
    state.runtime.runs[0].status = "running".into();
    state.runtime.runs[0].attempts.push(Attempt {
        workspace_id: Some("generation".into()),
        ..Default::default()
    });
    let id = state.runtime.runs[0].id.clone();
    let caller = Caller::Attempt {
        run: &id,
        generation: "generation",
    };
    assert!(authorize_write(&state, caller).is_ok());
    let mut mailbox = Mailbox::default();
    mailbox
        .stage(
            "draft".into(),
            Handoff {
                base_revision: 0,
                payload: Payload {
                    subgoal: "Retain progress".into(),
                    ..Default::default()
                },
                processed: vec![],
                resynchronised: vec![],
                disposition: Disposition::Continue,
                progress: None,
            },
            0,
        )
        .unwrap();
    state.runtime.runs[0].attempts[0].turn_mailbox = Some(Box::new(mailbox)).into();
    assert!(
        authorize(&state, caller).is_ok(),
        "observations remain permitted"
    );
    assert!(
        authorize_write(&state, caller).is_err(),
        "later writes must recheck the handoff fence"
    );
    state.runtime.runs[0].attempts[0].turn_mailbox = None.into();
    state.runtime.runs[0].question = Some(crate::design::Question {
        id: "question".into(),
        generation: "generation".into(),
        nonce: "fixture".into(),
        input: crate::design::Input {
            question: "Choose a direction".into(),
            options: vec!["First".into(), "Second".into()],
            recommended: None,
        },
        answer: None,
        continuation: None,
    });
    assert!(authorize_write(&state, caller).is_err());
    state.runtime.runs[0].question.as_mut().unwrap().answer = Some("First".into());
    assert!(authorize_write(&state, caller).is_ok());
    let binding:Configuration=serde_json::from_value(json!({"repository":"example/project","source_root":"/source","token_file":"/private/token","base":"main"})).unwrap();
    assert!(
        !binding.allow_merge,
        "old bindings must not acquire merge permission"
    );
}
