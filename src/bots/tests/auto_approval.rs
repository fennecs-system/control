//! Bot requests under the node's automatic approval policy: approved at once
//! within the task's limits, recorded with the policy as decider, and left
//! with the operator otherwise.
use super::*;

fn auto() -> AutoApproval {
    AutoApproval {
        mode: Mode::Auto,
        ..Default::default()
    }
}
fn ask_with(state: &mut State, nonce: &str, chat: &str, policy: &AutoApproval) -> io::Result<Value> {
    let run = state.runtime.runs[0].id().as_str().to_owned();
    let attempt = AttemptRef {
        run: &run,
        generation: "attempt-a",
        nonce,
    };
    handle_with(
        state,
        &attempt,
        BotCall::Request {
            chat_id: chat.into(),
            every_minutes: Some(5),
            room: true,
        },
        |_| Ok(()),
        policy,
    )
}

#[test]
fn a_bot_within_the_task_limits_is_approved_by_the_policy() {
    let (mut state, parent, child) = requester();
    let value = ask_with(&mut state, "n1", &child, &auto()).unwrap();
    assert_eq!(value["decision"], "approved", "{value}");
    assert_eq!(
        (value["decided_by"].as_str(), value["rule"].as_str()),
        (Some("policy"), Some("bot-in-task-limits"))
    );
    assert!(
        state.bots.persists(&child),
        "the bot runs as if the operator allowed it"
    );
    assert_eq!(pending_count(&state, &parent), 0);
    let record = state.auto_decisions.last().unwrap();
    assert_eq!(
        (record.kind.as_str(), record.decider.as_str(), record.chat.as_str()),
        ("bot", "policy", parent.as_str())
    );
    assert!(record.what.contains(&child));
    assert!(
        state
            .bots
            .notices
            .iter()
            .any(|notice| notice.id.starts_with("policy-bot-")),
        "an FYI notice"
    );
    // The requester already has the decision: no decision turn follows.
    finish_requesting_turn(&mut state);
    let runs = state.runtime.runs.len();
    assert_eq!(advance(&mut state), (false, false));
    assert_eq!(state.runtime.runs.len(), runs);
    // Revocation is the operator's usual /bot off.
    command_at(&mut state, &child, "off", Now(1)).unwrap();
    assert!(!state.bots.persists(&child));
}

#[test]
fn bots_over_the_limits_strict_mode_or_strict_principals_wait_for_the_operator() {
    for (policy, why) in [
        (AutoApproval::default(), "strict mode"),
        (
            AutoApproval {
                bot: control::permissions::KindMode::Ask,
                ..auto()
            },
            "bots ask",
        ),
        (
            AutoApproval {
                strict: vec!["local".into()],
                ..auto()
            },
            "strict principal",
        ),
    ] {
        let (mut state, _, child) = requester();
        let value = ask_with(&mut state, "n1", &child, &policy).unwrap();
        assert_eq!(value["decision"], "pending", "{why}");
        assert!(state.auto_decisions.is_empty() && !state.bots.persists(&child), "{why}");
    }
    let (mut state, parent, child) = requester();
    state.runtime.execution.max_agents_per_root = 1;
    let value = ask_with(&mut state, "n2", &child, &auto()).unwrap();
    assert_eq!(value["decision"], "pending", "the task is over its spawn limit");
    assert_eq!(pending_count(&state, &parent), 1);
    assert!(state.auto_decisions.is_empty());
}

#[test]
fn a_failed_save_rolls_back_an_automatic_approval() {
    let (mut state, _, child) = requester();
    let run = state.runtime.runs[0].id().as_str().to_owned();
    let attempt = AttemptRef {
        run: &run,
        generation: "attempt-a",
        nonce: "n1",
    };
    let call = BotCall::Request {
        chat_id: child.clone(),
        every_minutes: Some(5),
        room: true,
    };
    assert!(
        handle_with(
            &mut state,
            &attempt,
            call,
            |_| Err(io::Error::other("disk full")),
            &auto()
        )
        .is_err()
    );
    assert!(!state.bots.persists(&child) && state.bots.approvals.is_empty());
    assert!(state.auto_decisions.is_empty() && state.bots.notices.is_empty());
}
