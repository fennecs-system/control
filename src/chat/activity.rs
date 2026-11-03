//! Bounded command evidence shared by web and terminal chat clients.
use crate::{State, escape, usage::Activity};
use std::collections::HashSet;

/// Refresh ancestors without changing conversation ordering or persisting each
/// output sample. A malformed saved parent cycle must not trap the supervisor.
pub(crate) fn changed(state: &mut State, id: &str) {
    let mut cursor = Some(id.to_owned());
    let mut visited = HashSet::new();
    while let Some(id) = cursor.take() {
        if !visited.insert(id.clone()) {
            break;
        }
        let Some(index) = state.chats.iter().position(|chat| chat.id == id) else {
            break;
        };
        let chat = &mut state.chats[index];
        chat.revision = chat.revision.saturating_add(1);
        let actor = chat.actor.clone();
        cursor = chat.parent_chat.clone().filter(|parent| {
            state
                .chats
                .iter()
                .any(|chat| chat.id == *parent && chat.actor == actor)
        });
    }
}

#[derive(serde::Serialize)]
pub(super) struct AgentActivity<'a> {
    pub chat: &'a str,
    pub activity: Activity,
}

pub(super) fn agent_activity<'a>(state: &'a State, id: &str) -> Vec<AgentActivity<'a>> {
    let Some(session) = state.chats.iter().find(|chat| chat.id == id) else {
        return Vec::new();
    };
    // A service grant permits execution, not viewing another conversation's
    // history. Follow only same-actor chat ancestry, never arbitrary grants.
    state
        .runtime
        .runs
        .iter()
        .filter(|run| run.status == "running" && run.actor == session.actor)
        .filter_map(|run| {
            let child = run.chat_id.as_deref()?;
            if child == id {
                return None;
            }
            let mut cursor = child;
            let mut seen = HashSet::new();
            while seen.insert(cursor) {
                let chat = state
                    .chats
                    .iter()
                    .find(|chat| chat.id == cursor && chat.actor == session.actor)?;
                let parent = chat.parent_chat.as_deref()?;
                if parent == id {
                    return Some(
                        run.attempts
                            .last()
                            .into_iter()
                            .flat_map(|attempt| attempt.activity.iter().rev())
                            .filter(|event| event.kind == "message")
                            .take(1)
                            .cloned()
                            .chain(
                                crate::services::command_activity(state, &run.id)
                                    .rev()
                                    .take(1),
                            )
                            .map(move |mut activity| {
                                activity.id = format!("{}-{}", run.id, activity.id);
                                AgentActivity {
                                    chat: child,
                                    activity,
                                }
                            }),
                    );
                }
                cursor = parent;
            }
            None
        })
        .flatten()
        .take(16)
        .collect()
}

pub(super) fn command_html(event: &Activity, open: bool) -> String {
    if event.kind == "message" {
        return format!(
            "<article class='assistant-message' data-transcript-key='message-{}'><p class='preserve'>{}</p></article>",
            escape(&event.id),
            escape(&event.text)
        );
    }
    format!(
        "<details class='command-activity' data-transcript-key='{}'{}><summary><span>{}</span> <code>{}</code></summary><pre>{}</pre></details>",
        escape(&event.id),
        if open { " open" } else { "" },
        escape(&event.status),
        escape(&event.text),
        escape(&event.output)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn child_chat_messages_are_inline_scoped_and_refresh_ancestors() {
        let mut state = State::default();
        let parent = crate::chat::create(&mut state);
        let child = crate::chat::create(&mut state);
        state.chats[1].parent_chat = Some(parent.clone());
        crate::chat::send(&mut state, &child, "Check inventory", "test").unwrap();
        let run = &mut state.runtime.runs[0];
        let run_id = run.id.clone();
        run.status = "running".into();
        run.attempts.push(crate::usage::Attempt {
            workspace_id: Some("generation".into()),
            ..Default::default()
        });
        let revision = state.chats[0].revision;
        let event = Activity {
            id: "message-1".into(),
            kind: "message".into(),
            text: "Checking <inventory>.".into(),
            output: String::new(),
            status: "running".into(),
        };
        assert!(crate::worker::record_activity_for(
            &mut state,
            &run_id,
            "generation",
            event
        ));
        assert!(state.chats[0].revision > revision);
        let html = crate::chat::transcript(&state, &parent).unwrap();
        assert!(html.contains("<p class='preserve'>Checking &lt;inventory&gt;.</p>"));
        assert!(!html.contains("<pre>Checking"));
        let detail = crate::chat::detail(&state, &parent).unwrap();
        assert_eq!(detail["agent_activity"][0]["activity"]["kind"], "message");
        let mut stored: State =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(
            crate::chat::transcript(&stored, &child)
                .unwrap()
                .contains("Checking &lt;inventory&gt;.")
        );
        stored.runtime.runs[0].result = "Checking <inventory>.".into();
        assert_eq!(
            crate::chat::transcript(&stored, &child)
                .unwrap()
                .matches("Checking &lt;inventory&gt;.")
                .count(),
            1,
            "Do not repeat the final answer as a progress message"
        );
        stored.chats[1].actor = crate::identity::Actor::Unknown;
        assert!(agent_activity(&stored, &parent).is_empty());
    }
}
