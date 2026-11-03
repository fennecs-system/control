//! The conversation history interface: the one way frontends (web, TUI,
//! Discord, future bridges) and compactors read a chat, whatever stores its
//! messages, runs and journals.
//!
//! A conversation is a comonad space of entries (a message and the run that
//! answered it). A [`Compactor`] chooses which entries a model turn sees; the
//! default, [`Recent`], is Control's long-standing character window, so
//! behaviour is unchanged unless a node picks another. Providers' own
//! compaction (Codex, Claude Code) is untouched either way.
use crate::{chat::{Message, Session}, runtime::Run, State};
use control_comonad::{cut, Block, Space};
use std::collections::HashMap;

/// A chat's entries, with every answering run indexed once.
pub(crate) struct Conversation<'a> {
    session: &'a Session,
    runs: HashMap<&'a str, &'a Run>,
}

/// One message and the run that answered it, if any.
#[derive(Clone, Copy)]
pub(crate) struct Entry<'a> {
    pub message: &'a Message,
    pub run: Option<&'a Run>,
}
impl<'a> Entry<'a> {
    /// The run's status and reply, or the message's own reply without a run.
    pub fn response(&self) -> (&'a str, &'a str) {
        crate::chat::response_from_run(self.message, self.run)
    }
    pub fn speaker(&self) -> &'static str {
        match self.run.map_or(control_context::agent_policy::MessageSpeaker::User, |r| r.kind.speaker()) {
            control_context::agent_policy::MessageSpeaker::User => "User",
            control_context::agent_policy::MessageSpeaker::Control => "Control event",
        }
    }
    /// The entry as prompt history shows it.
    pub fn render(&self) -> String {
        format!("{}: {}\nAssistant: {}\n", self.speaker(), self.message.text, self.response().1)
    }
}

impl<'a> Conversation<'a> {
    pub fn of(state: &'a State, session: &'a Session) -> Self {
        let mut wanted: HashMap<&str, Option<&Run>> = session.messages.iter().filter_map(|m| m.run_id.as_deref()).map(|id| (id, None)).collect();
        let mut remaining = wanted.len();
        for run in &state.runtime.runs {
            if remaining == 0 {
                break;
            }
            // First match keeps the transcript's long-standing lookup semantics.
            if let Some(slot) = wanted.get_mut(run.id.as_str()).filter(|slot| slot.is_none()) {
                *slot = Some(run);
                remaining -= 1;
            }
        }
        Self { session, runs: wanted.into_iter().filter_map(|(id, run)| Some((id, run?))).collect() }
    }
    pub fn len(&self) -> usize {
        self.session.messages.len()
    }
    pub fn entry(&self, index: usize) -> Option<Entry<'a>> {
        let message = self.session.messages.get(index)?;
        Some(Entry { message, run: message.run_id.as_deref().and_then(|id| self.runs.get(id).copied()) })
    }
    /// Where the message answered by `run` sits.
    /// The run answering one of this chat's messages.
    pub fn run(&self, id: &str) -> Option<&'a Run> {
        self.runs.get(id).copied()
    }
    pub fn position(&self, run: &str) -> Option<usize> {
        self.session.messages.iter().position(|m| m.run_id.as_deref() == Some(run))
    }
}

impl Space for Conversation<'_> {
    type Pos = usize;
    type Item<'b>
        = Entry<'b>
    where
        Self: 'b;
    fn at(&self, pos: usize) -> Option<Entry<'_>> {
        self.entry(pos)
    }
    fn positions(&self) -> impl Iterator<Item = usize> + '_ {
        0..self.len()
    }
}

/// What a model turn sees of the history before entry `current`.
pub(crate) enum Item {
    Entry(usize),
    Summary(Block, String),
}
pub(crate) struct Window {
    pub items: Vec<Item>,
    /// Earlier entries the turn does not see verbatim.
    pub omitted: usize,
}
impl Window {
    pub fn render(&self, conversation: &Conversation<'_>) -> Vec<String> {
        self.items.iter().filter_map(|item| match item {
            Item::Entry(index) => conversation.entry(*index).map(|entry| entry.render()),
            Item::Summary(block, text) => Some(format!("Summary of entries {}–{}: {text}\n", block.start + 1, block.end())),
        }).collect()
    }
}

/// Chooses the history a model turn sees. Pure: summaries are produced
/// elsewhere and offered through [`Summaries`].
pub(crate) trait Compactor: Send + Sync {
    fn window(&self, conversation: &Conversation<'_>, current: usize) -> Window;
}

/// The newest entries that fit a character budget (Control's default).
pub(crate) struct Recent {
    pub chars: usize,
}
impl Default for Recent {
    fn default() -> Self {
        Self { chars: 20_000 }
    }
}
impl Compactor for Recent {
    fn window(&self, conversation: &Conversation<'_>, current: usize) -> Window {
        let mut items = Vec::new();
        let mut count = 0;
        for index in (0..current.min(conversation.len())).rev() {
            let Some(entry) = conversation.entry(index) else { break };
            count += entry.render().chars().count();
            if count > self.chars {
                break;
            }
            items.push(Item::Entry(index));
        }
        items.reverse();
        Window { omitted: current - items.len(), items }
    }
}

/// Every earlier entry; the provider's own compaction decides what fits.
pub(crate) struct Passthrough;
impl Compactor for Passthrough {
    fn window(&self, conversation: &Conversation<'_>, current: usize) -> Window {
        Window { items: (0..current.min(conversation.len())).map(Item::Entry).collect(), omitted: 0 }
    }
}

/// Block summaries for [`Lens`], computed once per fixed block elsewhere.
pub(crate) trait Summaries: Sync {
    fn summary(&self, block: Block) -> Option<String>;
}
/// No summaries yet: older blocks are omitted.
pub(crate) struct NoSummaries;
impl Summaries for NoSummaries {
    fn summary(&self, _: Block) -> Option<String> {
        None
    }
}

/// Unbounded history (UniiChat-style): the newest `verbatim` entries whole and
/// older history as O(log n) fixed blocks, each shown by its summary when one
/// exists. A block never changes once complete, so its summary is written once.
pub(crate) struct Lens<'s> {
    pub verbatim: u64,
    pub summaries: &'s dyn Summaries,
}
impl Compactor for Lens<'_> {
    fn window(&self, conversation: &Conversation<'_>, current: usize) -> Window {
        let current = current.min(conversation.len());
        let mut items = Vec::new();
        let mut omitted = 0;
        for block in cut(current as u64, self.verbatim) {
            if block.is_item() {
                items.push(Item::Entry(block.start as usize));
            } else if let Some(text) = self.summaries.summary(block) {
                items.push(Item::Summary(block, text));
                omitted += block.len() as usize;
            } else {
                omitted += block.len() as usize;
            }
        }
        items.reverse();
        Window { items, omitted }
    }
}

/// The compactor a node uses for prompt history: `history.json` in the node's
/// configuration, e.g. `{"compactor": "lens", "verbatim": 32}`. Missing or
/// unreadable settings keep the default, [`Recent`].
pub(crate) fn compactor() -> &'static dyn Compactor {
    #[derive(serde::Deserialize, Default)]
    #[serde(rename_all = "lowercase")]
    enum Kind {
        #[default]
        Recent,
        Passthrough,
        Lens,
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default, deny_unknown_fields)]
    struct Settings {
        compactor: Kind,
        verbatim: Option<u64>,
    }
    static CHOSEN: std::sync::OnceLock<Box<dyn Compactor>> = std::sync::OnceLock::new();
    CHOSEN.get_or_init(|| {
        let settings = crate::config::directory().ok()
            .and_then(|dir| crate::config::read_regular(&dir.join("history.json"), 4096, true).ok())
            .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
            .unwrap_or_default();
        match settings.compactor {
            Kind::Recent => Box::new(Recent::default()),
            Kind::Passthrough => Box::new(Passthrough),
            Kind::Lens => Box::new(Lens { verbatim: settings.verbatim.unwrap_or(32), summaries: &NoSummaries }),
        }
    }).as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_comonad::Store;

    fn chat(turns: usize, text: usize) -> (State, String) {
        let mut state = State::default();
        let id = crate::chat::create(&mut state);
        for turn in 0..turns {
            crate::chat::send(&mut state, &id, &format!("turn {turn} {}", "x".repeat(text)), "test").unwrap();
            let run = state.runtime.runs.last_mut().unwrap();
            run.status = "completed".into();
            *run.result = format!("reply {turn}");
        }
        (state, id)
    }
    fn session<'a>(state: &'a State, id: &str) -> &'a Session {
        state.chats.iter().find(|s| s.id == id).unwrap()
    }
    fn entries(window: &Window) -> Vec<usize> {
        window.items.iter().filter_map(|item| match item { Item::Entry(i) => Some(*i), Item::Summary(..) => None }).collect()
    }

    #[test]
    fn recent_keeps_the_newest_entries_within_its_budget() {
        let (state, id) = chat(40, 900);
        let conversation = Conversation::of(&state, session(&state, &id));
        let window = Recent::default().window(&conversation, 39);
        let kept = entries(&window);
        assert_eq!(kept.last(), Some(&38), "ends just before the current entry");
        assert_eq!(window.omitted + kept.len(), 39);
        assert!(window.render(&conversation).concat().chars().count() <= 20_000);
        let one_more = conversation.entry(kept[0] - 1).unwrap().render().chars().count();
        assert!(window.render(&conversation).concat().chars().count() + one_more > 20_000, "as many as fit");
        assert!(window.render(&conversation)[0].starts_with("User: turn") && window.render(&conversation)[0].contains("Assistant: reply"));
    }

    #[test]
    fn passthrough_and_lens_compose_over_the_same_entries() {
        struct Fixed;
        impl Summaries for Fixed {
            fn summary(&self, block: Block) -> Option<String> {
                Some(format!("{} earlier turns", block.len()))
            }
        }
        let (state, id) = chat(100, 10);
        let conversation = Conversation::of(&state, session(&state, &id));
        let all = Passthrough.window(&conversation, 99);
        assert_eq!((entries(&all).len(), all.omitted), (99, 0));
        let lens = Lens { verbatim: 8, summaries: &Fixed }.window(&conversation, 99);
        let kept = entries(&lens);
        assert!(kept.ends_with(&(91..99).collect::<Vec<_>>()), "at least the newest 8 verbatim: {kept:?}");
        let rendered = lens.render(&conversation);
        assert!(rendered[0].starts_with("Summary of entries 1–"), "oldest history first, summarised");
        assert!(rendered.len() < 30, "O(log n) items for 99 entries: {}", rendered.len());
        let bare = Lens { verbatim: 8, summaries: &NoSummaries }.window(&conversation, 99);
        assert_eq!(entries(&bare).len() + bare.omitted, 99, "without summaries older blocks are omitted");
        assert!(bare.omitted >= 80);
        assert_eq!(Store::new(&conversation, 3).unwrap().extract().message.text, "turn 3 xxxxxxxxxx", "a conversation is a comonad space");
    }
}
