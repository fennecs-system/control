use super::*;
use crate::{
    application::runs::Submission,
    identity::{Action, Actor},
};

fn run(id: &str, chat: &str, status: &str, kind: RunKind) -> Run {
    let mut run = Submission {
        objective: "task".into(),
        kind,
        source: "test".into(),
        actor: Actor::LocalOperator,
        action: Action::Task,
    }
    .into_run(id.into(), 1, Default::default(), None);
    run.chat_id = Some(chat.into());
    run.status = status.into();
    run
}

#[test]
fn live_turns_defer_even_with_a_newer_completed_turn_or_missing_request() {
    for status in ["queued", "running", "waiting", "cancelling"] {
        let runs = [
            run("origin", "chat", status, RunKind::Task),
            run("later", "chat", "completed", RunKind::BotWake),
        ];
        assert_eq!(decide(&runs, "chat", &["origin"]), Eligibility::Wait);
        assert_eq!(decide(&runs, "chat", &[]), Eligibility::Wait);
        assert_eq!(decide(&runs, "other", &["origin"]), Eligibility::Note);
    }
}

#[test]
fn later_human_steering_is_relative_to_the_latest_matching_request() {
    let mut runs = vec![
        run("origin", "chat", "completed", RunKind::Task),
        run("steering", "chat", "completed", RunKind::Task),
        run("answer", "chat", "completed", RunKind::DesignAnswer),
    ];
    assert_eq!(decide(&runs, "chat", &["origin"]), Eligibility::Note);
    assert_eq!(
        decide(&runs, "chat", &["origin", "steering"]),
        Eligibility::Candidate(2)
    );
    runs.push(run("unrelated", "other", "running", RunKind::Task));
    assert_eq!(
        decide(&runs, "chat", &["steering"]),
        Eligibility::Candidate(2)
    );
    runs.push(run("new-steering", "chat", "completed", RunKind::Task));
    assert_eq!(decide(&runs, "chat", &["steering"]), Eligibility::Note);
}

#[test]
fn missing_requests_and_unsuccessful_latest_turns_are_not_resumed() {
    for status in ["failed", "interrupted", "cancelled", "blocked"] {
        let runs = [
            run("origin", "chat", "completed", RunKind::Task),
            run("later", "chat", status, RunKind::BotWake),
        ];
        assert_eq!(decide(&runs, "chat", &["origin"]), Eligibility::Note);
    }
    let runs = [run("origin", "chat", "completed", RunKind::Task)];
    assert_eq!(decide(&runs, "chat", &["missing"]), Eligibility::Note);
    assert_eq!(decide(&[], "chat", &["origin"]), Eligibility::Note);
}

#[test]
fn recording_uses_only_supplied_records_and_preserves_unrelated_chats() {
    let mut runs = vec![run("origin", "chat", "completed", RunKind::Task)];
    let mut notice = "unchanged".to_owned();
    let session = |id, revision| serde_json::from_value::<Session>(serde_json::json!({"id":id,"revision":revision,"actor":{"type":"local_operator"},"title":id,"messages":[]})).unwrap();
    let mut chats = vec![session("chat", u64::MAX), session("other", 0)];
    let other = serde_json::to_vec(&chats[1]).unwrap();
    assert_eq!(record(Edit { runs: &mut runs, notice: &mut notice, chats: &mut chats }, "chat", None, "denied".into()), Outcome::Noted);
    assert_eq!(runs.len(), 1);
    assert_eq!(notice, "unchanged");
    assert_eq!(chats[0].messages[0].run_id, None);
    assert_eq!(chats[0].revision, u64::MAX);
    let continuation = run("resumed", "chat", "queued", RunKind::PortDecision);
    assert_eq!(record(Edit { runs: &mut runs, notice: &mut notice, chats: &mut chats }, "chat", Some(continuation), "approved".into()), Outcome::Resumed("resumed".into()));
    assert_eq!(runs.last().unwrap().id, "resumed");
    assert_eq!(chats[0].messages[1].run_id.as_deref(), Some("resumed"));
    assert_eq!(notice, "Run queued.");
    assert_eq!(serde_json::to_vec(&chats[1]).unwrap(), other);
}
