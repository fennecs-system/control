//! `/use` lens arguments: one parser shared by submission and live highlighting.
//! Validity always comes from the server's lens library (`control.lenses.list`),
//! the same definitions `control.chat.lenses` resolves against; no IDs live here.
use serde_json::Value;
use std::{collections::HashSet, ops::Range};

const PREFIX: &str = "/use";

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

fn separator(c: char) -> bool { c.is_whitespace() || c == ',' }

/// The argument text of a `/use` command, without scanning it.
fn arguments(text: &str) -> Option<&str> {
    text.strip_prefix(PREFIX).filter(|tail| tail.starts_with(char::is_whitespace))
}

/// Whether `text` is a `/use` command; a constant-time prefix check.
pub(super) fn is_use(text: &str) -> bool { arguments(text).is_some() }

/// Byte ranges of each lens name in a `/use` command. Names are separated by
/// whitespace and/or commas, so `/use a b`, `/use a,b` and `/use a, b` agree.
fn ranges(text: &str) -> Option<impl Iterator<Item = Range<usize>> + '_> {
    let tail = arguments(text)?;
    Some(tail.split(separator).filter(|name| !name.is_empty()).map(move |name| {
        let start = name.as_ptr() as usize - text.as_ptr() as usize;
        start..start + name.len()
    }))
}

pub(super) fn names(text: &str) -> Option<Vec<Range<usize>>> { ranges(text).map(Iterator::collect) }

/// The lenses a `/use` command selects; `/use none` clears them.
pub(super) fn parse(text: &str) -> Option<Vec<&str>> {
    let names: Vec<_> = ranges(text)?.map(|range| &text[range]).collect();
    Some(if names == ["none"] { Vec::new() } else { names })
}

/// Selected names absent from the library, in input order without repeats.
pub(super) fn unknown<'a>(names: &[&'a str], known: impl Fn(&str) -> bool) -> Vec<&'a str> {
    let mut missing = Vec::new();
    for name in names.iter().copied().filter(|name| !known(name)) {
        if !missing.contains(&name) { missing.push(name); }
    }
    missing
}

/// Live highlighting for a `/use` draft: each name and whether it is a known
/// lens (`none` alone is the valid clear form). `None` if the line is not `/use`.
pub(super) fn highlight<'a>(text: &'a str, known: impl Fn(&str) -> bool + 'a) -> Option<impl Iterator<Item = (Range<usize>, bool)> + 'a> {
    let clear = arguments(text)?.trim_matches(separator) == "none";
    Some(ranges(text)?.map(move |range| { let valid = clear || known(&text[range.clone()]); (range, valid) }))
}

#[cfg(test)]
mod tests;
