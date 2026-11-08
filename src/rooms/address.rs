//! Room addressing is coordination policy, not a supervisor concept. An exact
//! address reuses the existing durable room inbox, cursor and wake machinery.
use crate::State;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Address<'a> {
    pub channel: &'a str,
    pub thread: Option<&'a str>,
}
impl<'a> Address<'a> {
    pub fn parse(value: &'a str) -> Option<Self> {
        let (channel, thread) = value.split_once('/').map_or((value, None), |(channel, thread)| (channel, Some(thread)));
        (super::NAMES.contains(&channel) && thread.is_none_or(crate::chat::valid_id))
            .then_some(Self { channel, thread })
    }
}

/// A new operator task gets a new thread. Selecting an existing thread is an
/// explicit operator choice to collaborate there instead.
pub(crate) fn for_task(room: &str, chat: &str) -> Result<String, &'static str> {
    let address = Address::parse(room).ok_or("Unknown room.")?;
    Ok(if address.thread.is_some() { room.to_owned() } else { format!("{room}/{chat}") })
}

/// Bare names are aliases inside the task's inherited thread. A model cannot
/// read or post into another task by supplying its thread ID. Already-linked
/// legacy shared rooms retain their addressing until the operator moves them.
pub(super) fn for_worker(state: &State, chat: &str, room: &str) -> Result<String, &'static str> {
    let requested = Address::parse(room).ok_or("Unknown room. Call control_room_list.")?;
    let mut current = chat;
    let mut inherited = None;
    for _ in 0..64 {
        let session = state.chats.iter().find(|session| session.id == current).ok_or("Chat lineage is unavailable.")?;
        inherited = inherited.or_else(|| session.room.as_deref().and_then(Address::parse));
        match session.parent_chat.as_deref() {
            Some(parent) => current = parent,
            None => {
                let thread = inherited.and_then(|address| address.thread).unwrap_or(current);
                if requested.thread.is_some_and(|value| value != thread) {
                    return Err("Room belongs to another task thread. Use a channel name in your own thread.");
                }
                return Ok(if requested.thread.is_none() && inherited.is_some_and(|address| address.thread.is_none()) {
                    room.to_owned()
                } else {
                    format!("{}/{thread}", requested.channel)
                });
            }
        }
    }
    Err("Chat lineage is too deep or cyclic.")
}

pub(crate) fn names(state: &State) -> Vec<String> {
    let mut names = super::NAMES.map(str::to_owned).to_vec();
    for room in state.chats.iter().rev().filter_map(|chat| chat.room.as_deref())
        .chain(state.collective.posts.iter().rev().map(|post| post.room.as_str())) {
        if Address::parse(room).is_some() && !names.iter().any(|name| name == room) {
            names.push(room.to_owned());
        }
    }
    names
}

pub(super) fn label(state: &State, room: &str) -> String {
    let Some(address) = Address::parse(room) else { return room.to_owned(); };
    match address.thread {
        Some(thread) => {
            let title = state.chats.iter().find(|chat| chat.id == thread).map_or(thread, |chat| chat.title.as_str());
            format!("#{} · {}", address.channel, title.chars().take(60).collect::<String>())
        }
        None => format!("#{}", address.channel),
    }
}
