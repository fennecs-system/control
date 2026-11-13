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
