use crate::{
    State,
    accounts::{AccountName, Ended, UnixSeconds},
    backend::Backend,
    runtime::RunStatus,
};

const CAPPED: &str = "Held: codex account capped is running its limit of 1 at once.";
const PROBING: &str = "Held: codex account probed reached its usage limit; one turn is checking whether it has lifted. Clear on the providers page or run `control accounts clear codex probed`.";

/// One chat whose queued turns are held two ways: by a capped account that
/// runs its one attempt, and by a cooled account whose retry time has passed
/// while another turn probes it. `finished` completed runs are added after.
fn fixture(finished: usize) -> (State, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    for text in [
        "Capped and running",
        "Capped and queued",
        "Probe and running",
        "Probed and queued",
    ] {
        crate::chat::send(&mut state, &chat, text, "test").unwrap();
    }
    state.runtime.accounts = crate::accounts::Store::parse(
        br#"{"accounts":[{"backend":"codex","name":"capped","kind":"subscription","cap":1},{"backend":"codex","name":"probed","kind":"subscription"}]}"#,
    )
    .unwrap();
    let account = |name: &str| crate::providers::Target {
        backend: Backend::Codex,
        account: AccountName::try_from(name.to_owned()).unwrap(),
        ..Default::default()
    };
    for (index, (name, status)) in [
        ("capped", RunStatus::Running),
        ("capped", RunStatus::Queued),
        ("probed", RunStatus::Running),
        ("probed", RunStatus::Queued),
    ]
    .into_iter()
    .enumerate()
    {
        let run = &mut state.runtime.runs[index];
        run.target = account(name);
        run.status = status;
    }
    let long_ago = UnixSeconds(super::now().0 - 7200);
    let runtime = &mut state.runtime;
    runtime.account_status = runtime.account_status.ended(
        Backend::Codex,
        &AccountName::try_from("probed".to_owned()).unwrap(),
        long_ago,
        Ended::Limited(None),
        long_ago,
    );
    let template = state.runtime.runs[0].clone();
    for index in 0..finished {
        let mut run = template.clone();
        run.id = format!("finished-{index}");
        run.status = RunStatus::Completed;
        state.runtime.runs.push(run);
    }
    (state, chat)
}

/// What every interface shows for held turns, pinned before the hold facts
/// were computed once per listing instead of once per run.
#[test]
fn held_turns_read_the_same_in_every_view() {
    let (state, chat) = fixture(3);
    let list = crate::chat::list(&state);
    assert_eq!(list[0]["held"], CAPPED, "{list}");
    let detail = crate::chat::detail(&state, &chat).unwrap();
    let held = detail["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["held"].as_str().unwrap_or(""))
        .collect::<Vec<_>>();
    assert_eq!(held, ["", CAPPED, "", PROBING]);
    let transcript = crate::chat::transcript(&state, &chat).unwrap();
    assert_eq!(
        transcript
            .matches(&format!("<p class='message-status'>{}</p>", crate::escape(CAPPED)))
            .count(),
        1,
        "{transcript}"
    );
    assert_eq!(
        transcript
            .matches(&format!("<p class='message-status'>{}</p>", crate::escape(PROBING)))
            .count(),
        1
    );
    let notices = super::held_notices(&state.runtime);
    assert_eq!(notices, [(3, Some(PROBING.to_owned()))]);
    assert_eq!(crate::scheduler::next_in(&state), None, "both queued turns are held");
    let notes = super::notes(&state.runtime, super::now());
    assert_eq!(notes.len(), 1);
    assert_eq!(
        notes[0].note,
        "cooling: one turn is checking whether the limit has lifted, 1 turn held"
    );
}
