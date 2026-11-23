//! Live highlighting for `/use` drafts, built on the shared `/use` grammar in
//! `client::chats` that every client submits with.
//! Validity always comes from the server's lens library (`control.lenses.list`),
//! the same definitions `control.chat.lenses` resolves against; no IDs live here.
use serde_json::Value;
use control_client::chats;
use std::{collections::HashSet, ops::Range};

/// Lens IDs fetched once per `/use` draft and kept in memory for every redraw.
/// `Unavailable` keeps a failed lookup from retrying on every keystroke and
/// leaves names unstyled.
#[derive(Default)]
pub(super) enum Catalog {
    #[default]
    Unloaded,
    Unavailable,
    Known(HashSet<String>),
}

impl Catalog {
    pub(super) fn from_list(definitions: &Value) -> Self {
        definitions.as_array().map_or(Self::Unavailable, |definitions| {
            Self::Known(definitions.iter().filter_map(|d| d["id"].as_str()).map(str::to_owned).collect())
        })
    }
    pub(super) fn known(&self) -> Option<&HashSet<String>> {
        match self {
            Self::Known(ids) => Some(ids),
            _ => None,
        }
    }
}

/// Whether `text` is a `/use` command with arguments being typed; a
/// constant-time prefix check.
pub(super) fn is_use(text: &str) -> bool { chats::use_arguments(text).is_some_and(|arguments| !arguments.is_empty()) }

/// Byte ranges of each lens name in a `/use` command, from the shared
/// grammar, so `/use a b`, `/use a,b` and `/use a, b` agree with submission.
fn ranges(text: &str) -> Option<impl Iterator<Item = Range<usize>> + '_> {
    let tail = chats::use_arguments(text)?;
    Some(chats::lens_names(tail).map(move |name| {
        let start = name.as_ptr() as usize - text.as_ptr() as usize;
        start..start + name.len()
    }))
}

#[cfg(test)]
pub(super) fn names(text: &str) -> Option<Vec<Range<usize>>> { ranges(text).map(Iterator::collect) }

/// Live highlighting for a `/use` draft: each name and whether it is a known
/// lens (`none` alone is the valid clear form). `None` if the line is not `/use`.
pub(super) fn highlight<'a>(text: &'a str, known: impl Fn(&str) -> bool + 'a) -> Option<impl Iterator<Item = (Range<usize>, bool)> + 'a> {
    let clear = chats::lenses(chats::use_arguments(text)?).is_some_and(|names| names.is_empty());
    Some(ranges(text)?.map(move |range| { let valid = clear || known(&text[range.clone()]); (range, valid) }))
}

#[cfg(test)]
mod tests;
