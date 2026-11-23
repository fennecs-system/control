//! `/memory` (alias `/compact`): one DM's Memory Tree status, and the
//! operator's per-DM opt-in (`/memory on [MODEL]`) or opt-out (`/memory off`).
//! Changes go through the Models page's validation and save path.
use crate::{State, identity::Action};

pub(crate) const USAGE: &str = "Use /memory, /memory on [MODEL] or /memory off.";

/// One or two lines: on/off, which compactor and why, entries verbatim vs
/// folded, and where to inspect the tree.
pub(crate) fn summary(runtime: &crate::runtime::Runtime, session: &crate::chat::Session) -> String {
    let memory = &runtime.compaction;
    let (model, reason) = super::policy(runtime, session);
    let url = super::memory_url(&session.id);
    let total = session.messages.len();
    let Some(model) = model else {
        let next = if reason == "opted out" {
            "/memory on opts back in"
        } else {
            "/memory on [MODEL] opts in"
        };
        return format!("Memory Tree off for this DM ({reason}): {total} entries, none folded; {next}. {url}");
    };
    let folded = memory.views.get(&session.id).map_or(0, |view| {
        view.count.saturating_sub(memory.settings.recent_entries as usize)
    });
    let mut text = format!(
        "Memory Tree on for this DM: compactor {} ({reason}). {} entries verbatim, {folded} folded into summaries. {url}",
        model.id,
        total.saturating_sub(folded)
    );
    if memory.pending.as_ref().is_some_and(|(id, ..)| *id == session.id) {
        text.push_str("\nSummarizing now.");
    } else if let Some(error) = memory.errors.get(&session.id) {
        text.push_str(&format!("\nLast summary attempt failed: {error}"));
    }
    text
}

pub(crate) fn command(state: &mut State, chat: &str, option: &str) -> Result<String, &'static str> {
    command_with(state, chat, option, crate::providers::save_settings)
}

pub(crate) fn command_with(
    state: &mut State,
    chat: &str,
    option: &str,
    save: impl FnOnce(&crate::runtime::Runtime) -> std::io::Result<()>,
) -> Result<String, &'static str> {
    let session = state.chats.iter().find(|c| c.id == chat).ok_or("Chat not found.")?;
    crate::identity::authorize(&session.actor, Action::Task)?;
    let mut words = option.split_whitespace();
    let (on, named) = match (words.next(), words.next(), words.next()) {
        (None, ..) => return Ok(summary(&state.runtime, session)),
        (Some("on"), named, None) => (true, named),
        (Some("off"), None, None) => (false, None),
        _ => return Err(USAGE),
    };
    crate::identity::authorize(&session.actor, Action::Improve)
        .map_err(|_| "Changing the Memory Tree is operator-only.")?;
    let memory = &state.runtime.compaction;
    let mut settings = memory.settings.clone();
    settings.opted_out.retain(|id| id != chat);
    settings.chats.retain(|id| id != chat);
    if on {
        let configured = |id: &str| memory.models.iter().any(|m| m.id == id);
        let model = match named {
            Some(name) if configured(name) => name.to_owned(),
            Some(_) => {
                return Err(
                    "Unknown text model. Use /memory on MODEL with a text model configured in Manage → Models.",
                );
            }
            None => {
                let wanted = super::local_endpoint(&state.runtime, session)
                    .map(|m| m.id.clone())
                    .or_else(|| settings.model.clone().filter(|id| configured(id)))
                    .ok_or("No Memory Tree model is configured. Use /memory on MODEL with a configured text model.")?;
                // One compactor serves every opted-in DM: switching it is explicit.
                if settings.enabled
                    && !settings.chats.is_empty()
                    && settings.model.as_ref().is_some_and(|m| *m != wanted)
                {
                    return Err(
                        "Other opted-in DMs share the current Memory Tree model. Use /memory on MODEL to switch it for all of them.",
                    );
                }
                wanted
            }
        };
        settings.enabled = true;
        settings.model = Some(model);
        settings.chats.push(chat.to_owned());
    } else {
        settings.opted_out.push(chat.to_owned());
    }
    if settings != state.runtime.compaction.settings {
        crate::providers::save_compaction(&mut state.runtime, settings, save)?;
    }
    let session = state.chats.iter().find(|c| c.id == chat).ok_or("Chat not found.")?;
    Ok(summary(&state.runtime, session))
}
