//! Pure chat syntax shared by the server, terminal and WASM composer.
//! Parsing describes intent; callers retain all authorization and effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Improve,
    Steer,
    Btw,
    Spawn,
    Cancel,
    Answer,
    Open,
    Listen,
    Bot,
    Room,
}

pub struct Definition {
    pub id: &'static str,
    pub text: &'static str,
    pub description: &'static str,
    pub architect: bool,
}
pub const DEFINITIONS: &[Definition] = &[
    Definition {
        id: "improve",
        text: "/improve",
        description: "Discuss and change Control",
        architect: true,
    },
    Definition {
        id: "improve-off",
        text: "/improve off",
        description: "Return to regular chat",
        architect: true,
    },
    Definition {
        id: "steer",
        text: "/steer",
        description: "Queue a direction for the next turn",
        architect: false,
    },
    Definition {
        id: "btw",
        text: "/btw",
        description: "Ask in a separate conversation",
        architect: false,
    },
    Definition {
        id: "spawn",
        text: "/spawn",
        description: "Start an independent agent",
        architect: false,
    },
    Definition {
        id: "cancel",
        text: "/cancel",
        description: "Stop the current run",
        architect: false,
    },
    Definition {
        id: "answer",
        text: "/answer",
        description: "Answer a design question",
        architect: false,
    },
    Definition {
        id: "open",
        text: "/open",
        description: "Open a folder on this node",
        architect: false,
    },
    Definition { id:"listen", text:"/listen", description:"Listen for room messages", architect:false },
    Definition { id:"listen-off", text:"/listen off", description:"Stop room listening", architect:false },
    Definition { id:"bot", text:"/bot on", description:"Run this chat as a persistent bot", architect:false },
    Definition { id:"bot-off", text:"/bot off", description:"Stop this chat's bot", architect:false },
    Definition { id:"room", text:"/room general", description:"Attach this chat to a room", architect:false },
    Definition { id:"room-off", text:"/room off", description:"Detach this chat from its room", architect:false },
];

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed<'a> {
    pub kind: Kind,
    /// Original whitespace and argument spelling; interpreters choose when to trim.
    pub tail: &'a str,
    pub highlight: std::ops::Range<usize>,
}
pub fn parse(text: &str) -> Option<Parsed<'_>> {
    let trimmed = text.trim_start();
    let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let kind = match &trimmed[..end] {
        "/improve" => Kind::Improve,
        "/steer" => Kind::Steer,
        "/btw" => Kind::Btw,
        "/spawn" => Kind::Spawn,
        "/cancel" => Kind::Cancel,
        "/answer" => Kind::Answer,
        "/open" => Kind::Open,
        "/listen" => Kind::Listen,
        "/bot" => Kind::Bot,
        "/room" => Kind::Room,
        _ => return None,
    };
    let tail = &trimmed[end..];
    let suffix = if kind == Kind::Improve {
        [" off", " on"]
            .into_iter()
            .find(|suffix| {
                tail.strip_prefix(suffix)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
            })
            .map(str::len)
            .unwrap_or(0)
    } else {
        0
    };
    let start = text.len() - trimmed.len();
    Some(Parsed {
        kind,
        tail,
        highlight: start..start + end + suffix,
    })
}
pub fn tail(text: &str, kind: Kind) -> Option<&str> {
    parse(text)
        .filter(|command| command.kind == kind)
        .map(|command| command.tail)
}
pub fn completions(text: &str) -> Vec<usize> {
    if !text.starts_with('/') || text.contains('\n') {
        return Vec::new();
    }
    DEFINITIONS
        .iter()
        .enumerate()
        .filter_map(|(i, command)| command.text.starts_with(text).then_some(i))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn syntax_keeps_boundaries_arguments_and_unicode_offsets() {
        for definition in DEFINITIONS {
            assert!(parse(definition.text).is_some());
        }
        for text in [
            "/improvements",
            "/opener /open",
            "hello /spawn",
            "/spawned",
            "",
        ] {
            assert!(parse(text).is_none());
        }
        let text = "\u{2003}/open\t/tmp/猫";
        let command = parse(text).unwrap();
        assert_eq!(&text[command.highlight], "/open");
        assert_eq!(command.tail, "\t/tmp/猫");
        assert_eq!(
            tail("/spawn /improve change", Kind::Spawn),
            Some(" /improve change")
        );
        assert!(tail("/spawn /improve change", Kind::Improve).is_none());
        for text in ["/improve off", "/improve on request"] {
            let command = parse(text).unwrap();
            assert_eq!(
                &text[command.highlight],
                if text.ends_with("off") {
                    "/improve off"
                } else {
                    "/improve on"
                }
            );
        }
        assert_eq!(completions("/op"), vec![7]);
        assert!(completions("/open\n").is_empty());
    }
}
