//! Chat names. Every chat is shown by a callsign from a small fixed list,
//! chosen deterministically from its ID and distinct from the callsigns of the
//! chats created just before it; the operator may rename a chat (`/rename`).
//!
//! Callsigns are derived on read, never stored: a chat's callsign depends only
//! on its ID and the IDs of the chats before it, which are append-only, so it is
//! stable and old state needs no migration. Renames are stored on the chat and
//! do not move other chats' callsigns.
use crate::chat::Session;
use std::borrow::Cow;

/// The NATO phonetic alphabet, then colours.
pub const WORDS: &[&str] = &[
    "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel", "India", "Juliet", "Kilo", "Lima",
    "Mike", "November", "Oscar", "Papa", "Quebec", "Romeo", "Sierra", "Tango", "Uniform", "Victor", "Whiskey", "Xray",
    "Yankee", "Zulu", "Amber", "Azure", "Cobalt", "Coral", "Crimson", "Cyan", "Ebony", "Emerald", "Garnet", "Indigo",
    "Ivory", "Jade", "Lilac", "Magenta", "Maroon", "Ochre", "Olive", "Onyx", "Pearl", "Ruby", "Saffron", "Scarlet",
    "Sepia", "Teal", "Umber", "Violet",
];
/// How many preceding chats (live or recent) a new callsign must differ from.
const RECENT: usize = 48;
/// Longest operator-chosen name, in characters.
pub const MAX_NAME: usize = 32;

/// A word from the list, numbered from 2 only when every word is taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Callsign {
    word: u16,
    number: u32,
}
impl Callsign {
    fn text(self, words: &'static [&'static str]) -> Cow<'static, str> {
        let word = words[usize::from(self.word)];
        if self.number == 0 {
            Cow::Borrowed(word)
        } else {
            Cow::Owned(format!("{word} {}", self.number))
        }
    }
}

/// FNV-1a: fixed across builds and platforms, unlike the standard hasher.
fn start(id: &str, words: usize) -> usize {
    let hash = id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    (hash % words as u64) as usize
}

/// Callsigns of `ids` in creation order, each distinct from those of the
/// `recent` chats before it while the list has a free word.
fn assign<'a>(ids: impl Iterator<Item = &'a str>, words: &[&str], recent: usize, mut visit: impl FnMut(Callsign)) {
    // The window's plain words as counts, its rare numbered callsigns as a
    // short list, and the window itself as a ring: no hashing or allocation
    // per chat on this list-rendering path.
    let mut plain = vec![0u16; words.len()];
    let mut numbered: Vec<Callsign> = Vec::new();
    let mut ring = vec![Callsign { word: 0, number: 0 }; recent];
    for (index, id) in ids.enumerate() {
        let first = start(id, words.len());
        let callsign = (first..words.len())
            .chain(0..first)
            .find(|word| plain[*word] == 0)
            .map(|word| Callsign {
                word: word as u16,
                number: 0,
            })
            .unwrap_or_else(|| {
                (2..)
                    .map(|number| Callsign {
                        word: first as u16,
                        number,
                    })
                    .find(|c| !numbered.contains(c))
                    .expect("numbers are unbounded")
            });
        visit(callsign);
        if recent == 0 {
            continue;
        }
        let slot = index % recent;
        if index >= recent {
            let old = ring[slot];
            if old.number == 0 {
                plain[usize::from(old.word)] -= 1;
            } else if let Some(at) = numbered.iter().position(|c| *c == old) {
                numbered.swap_remove(at);
            }
        }
        if callsign.number == 0 {
            plain[usize::from(callsign.word)] += 1;
        } else {
            numbered.push(callsign);
        }
        ring[slot] = callsign;
    }
}

/// Every chat's callsign, indexed like `chats`.
pub fn all(chats: &[Session]) -> Vec<Callsign> {
    let mut all = Vec::with_capacity(chats.len());
    assign(chats.iter().map(|chat| chat.id.as_str()), WORDS, RECENT, |callsign| {
        all.push(callsign)
    });
    all
}

/// What clients show for chat `chats[index]`, given its callsign from [`all`].
pub fn label(chat: &Session, callsign: Callsign) -> Cow<'_, str> {
    chat.name.as_deref().map_or_else(|| callsign.text(WORDS), Cow::Borrowed)
}

/// The shown name of chat `id`; derives only the callsigns before it.
pub fn name<'a>(chats: &'a [Session], id: &str) -> Option<Cow<'a, str>> {
    let index = chats.iter().position(|chat| chat.id == id)?;
    if let Some(name) = chats[index].name.as_deref() {
        return Some(Cow::Borrowed(name));
    }
    let mut last = None;
    assign(
        chats[..=index].iter().map(|chat| chat.id.as_str()),
        WORDS,
        RECENT,
        |callsign| last = Some(callsign),
    );
    last.map(|callsign| callsign.text(WORDS))
}

/// [`name`], or the ID when the chat is missing.
pub fn name_or_id<'a>(chats: &'a [Session], id: &'a str) -> Cow<'a, str> {
    name(chats, id).unwrap_or(Cow::Borrowed(id))
}

/// An operator-chosen name: printable, at most [`MAX_NAME`] characters, with
/// surrounding and repeated whitespace collapsed.
pub fn validate(name: &str) -> Result<String, &'static str> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err("Usage: /rename NAME");
    }
    if name.chars().count() > MAX_NAME
        || name.chars().any(|c| {
            c.is_control()
                || matches!(c, '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
        })
    {
        return Err("Chat names are at most 32 printable characters.");
    }
    Ok(name)
}

#[cfg(test)]
mod tests;
