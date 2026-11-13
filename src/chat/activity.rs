//! Bounded command evidence shared by web and terminal chat clients.
use crate::{State, escape, usage::Activity};
use std::collections::HashSet;

/// Refresh ancestors without changing conversation ordering or persisting each
/// output sample. A malformed saved parent cycle must not trap the supervisor.
pub(crate) fn changed(state: &mut State, id: &str) {
    crate::live_updates::notify();
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
mod tests;
