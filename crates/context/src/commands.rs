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
    Model,
    Rename,
    /// `/memory [on [MODEL] | off]`: this DM's Memory Tree status or opt-in.
    Memory,
    /// `/compact`: the older spelling of a bare `/memory`.
    Compact,
    /// `/services [stop ID | all]`: list or stop the services of this DM's team.
    Services,
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
    Definition {
        id: "listen",
        text: "/listen",
        description: "Listen for room messages",
        architect: false,
    },
    Definition {
        id: "listen-off",
        text: "/listen off",
        description: "Stop room listening",
        architect: false,
    },
    Definition {
        id: "bot",
        text: "/bot on",
        description: "Run this DM as a persistent bot",
        architect: false,
    },
    Definition {
        id: "bot-off",
        text: "/bot off",
        description: "Stop this DM's bot",
        architect: false,
    },
    Definition {
        id: "room",
        text: "/room general",
        description: "Attach this DM to a room",
        architect: false,
    },
    Definition {
        id: "room-off",
        text: "/room off",
        description: "Detach this DM from its room",
        architect: false,
    },
    Definition {
        id: "cancel-all",
        text: "/cancel all",
        description: "Stop runs and bots in this DM and every DM it spawned",
        architect: false,
    },
    Definition {
        id: "model",
        text: "/model",
        description: "List or select this DM's model",
        architect: false,
    },
    Definition {
        id: "rename",
        text: "/rename",
        description: "Rename this DM",
        architect: false,
    },
    Definition {
        id: "memory",
        text: "/memory",
        description: "Show this DM's Memory Tree status (alias /compact)",
        architect: false,
    },
    Definition {
        id: "memory-on",
        text: "/memory on",
        description: "Opt this DM into the Memory Tree (operator)",
        architect: false,
    },
    Definition {
        id: "memory-off",
        text: "/memory off",
        description: "Opt this DM out of the Memory Tree (operator)",
        architect: false,
    },
    Definition {
        id: "services",
        text: "/services",
        description: "List the running services of this DM and its team",
        architect: false,
    },
    Definition {
        id: "services-stop",
        text: "/services stop",
        description: "Stop a service by ID (or all); /workspace is kept",
        architect: false,
    },
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
        "/model" => Kind::Model,
        "/rename" => Kind::Rename,
        "/memory" => Kind::Memory,
        "/compact" => Kind::Compact,
        "/services" => Kind::Services,
        _ => return None,
    };
    let tail = &trimmed[end..];
    let suffixes: &[&str] = match kind {
        Kind::Improve | Kind::Memory => &[" off", " on"],
        Kind::Services => &[" stop"],
        _ => &[],
    };
    let suffix = suffixes
        .iter()
        .copied()
        .find(|suffix| {
            tail.strip_prefix(suffix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        })
        .map_or(0, str::len);
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
mod tests;
