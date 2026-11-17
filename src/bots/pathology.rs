//! Pathology facts: signs that a team is working against its own goal, derived
//! cheaply from records the host already keeps. They join the lifecycle facts
//! an advisor hears for its team and an antibody hears for every team.
//!
//! - **Harness churn:** file- or tool-building commands keep running while the
//!   team's claims and score probes do not advance (building a generic harness
//!   instead of placing the stone wall).
//! - **Repetition loop:** one member's same command (digits folded) failed
//!   several times among its retained commands.
//! - **Polling:** one member's retained commands are mostly status-only reads.
//! - **Cost burn:** the team spent many tokens since it last advanced.
//!
//! Inputs are each service's retained finished commands (a bounded window,
//! `services::finished_commands`), run token counts and score probe samples.
//! Every fact is edge-triggered: reported once, and again only after the
//! condition clears. Commands are classified only when a team's retained set
//! changed, so a steady team does no text work. The host never parses prose or
//! decides what a workload's numbers mean; it reports, and the model judges.
use crate::rooms::claims::graph::Fx;
use serde::{Deserialize, Serialize};
use std::hash::Hasher;

/// Entries one fact list keeps.
const LISTED: usize = 8;
/// Bytes of a command compared or quoted.
const COMMAND: usize = 512;
/// Characters of a command quoted in a fact.
const EXCERPT: usize = 120;

/// File- or tool-building commands while the team did not advance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Churn {
    /// Building commands since the team last advanced.
    pub commands: u32,
    /// Members that ran them, bounded.
    pub members: Vec<String>,
}
/// One member's same command failing again and again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loop {
    pub chat: String,
    /// The command as run, shortened; untrusted.
    pub command: String,
    pub failures: u32,
}
/// One member issuing status-only commands instead of running a routine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Polling {
    pub chat: String,
    pub commands: u32,
}
/// Tokens spent since the team last advanced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Burn {
    pub tokens: u64,
}

/// What the host last observed of a team's pathologies.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Snapshot {
    /// Fingerprint of the team's retained command IDs last classified.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) commands: u64,
    /// Current repetition loops (member and command fingerprints), sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) loops: Vec<u64>,
    /// Members currently polling, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) polling: Vec<String>,
    /// Retained building commands already counted, sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) counted: Vec<String>,
    /// Members that ran the retained building commands, sorted and bounded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) builders: Vec<String>,
    /// Building commands since the team last advanced.
    #[serde(default, skip_serializing_if = "is_zero32")]
    pub(super) churn: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) churned: bool,
    /// Team tokens when it last advanced: the burn baseline.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) tokens: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) burned: bool,
    /// Unix time of the newest score probe sample that raised a value.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(super) score: u64,
}
fn is_zero(value: &u64) -> bool {
    *value == 0
}
fn is_zero32(value: &u32) -> bool {
    *value == 0
}

/// Thresholds from `execution.json`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    pub(super) churn: u32,
    pub(super) repeat: u32,
    pub(super) poll: u32,
    pub(super) burn: u64,
}
impl Limits {
    pub(super) fn of(limits: &crate::scheduler::Limits) -> Self {
        let small = |value: usize| u32::try_from(value).unwrap_or(u32::MAX).max(2);
        Self { churn: small(limits.pathology_churn_commands), repeat: small(limits.pathology_repeat_failures), poll: small(limits.pathology_poll_commands), burn: limits.pathology_burn_tokens.max(1) }
    }
}

/// One team's inputs for this pass.
#[derive(Default)]
pub(super) struct Scan<'a> {
    /// Retained finished commands of members: (member chat, command ID, text, failed).
    pub(super) commands: Vec<(&'a str, &'a str, &'a str, bool)>,
    /// Tokens of every recorded member attempt (judges excluded).
    pub(super) tokens: u64,
    /// Newest score sample that raised a value, as unix time.
    pub(super) score: u64,
}

/// Pathology facts found in one pass.
#[derive(Default)]
pub(super) struct Found {
    pub(super) churn: Option<Churn>,
    pub(super) loops: Vec<Loop>,
    pub(super) polling: Vec<Polling>,
    pub(super) burn: Option<Burn>,
}

/// The command with whitespace collapsed and digit runs folded, bounded, so
/// retries differing only in IDs, ports or counts compare equal.
fn normal(command: &str) -> String {
    let command = &command[..command.floor_char_boundary(COMMAND)];
    let mut out = String::with_capacity(command.len());
    let mut digits = false;
    for word in command.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        for c in word.chars() {
            if c.is_ascii_digit() {
                if !digits {
                    out.push('#');
                }
                digits = true;
            } else {
                digits = false;
                out.push(c);
            }
        }
        digits = false;
    }
    out
}
fn excerpt(command: &str) -> String {
    let command = command.split_whitespace().collect::<Vec<_>>().join(" ");
    match command.char_indices().nth(EXCERPT) {
        Some((end, _)) => format!("{}…", &command[..end]),
        None => command,
    }
}
/// Builders, compilers and package managers: making tools, not using them.
const BUILDERS: [&str; 22] = ["cargo", "rustc", "npm", "npx", "pnpm", "yarn", "pip", "pip3", "uv", "go", "make", "cmake", "gcc", "g++", "cc", "clang",
    "tsc", "javac", "mvn", "gradle", "patch", "nix-build"];
/// Whether a command writes files or builds tools: a redirect into a file
/// (not `/dev/null` or another descriptor), `tee`, in-place edits, `chmod +x`,
/// `git apply` or a builder. Status-only commands never count.
pub(super) fn builds(command: &str) -> bool {
    let command = &command[..command.floor_char_boundary(COMMAND * 4)];
    if crate::observe::status_style(command) {
        return false;
    }
    let redirect = command.split('>').skip(1).any(|after| {
        let target = after.trim_start_matches('>').trim_start();
        !target.starts_with('&') && !target.starts_with("/dev/null") && !after.starts_with('&') && !target.is_empty()
    });
    redirect || command.split(['|', ';', '&', '\n']).any(|part| {
        let mut words = part.split_whitespace().skip_while(|word| matches!(*word, "sudo" | "env" | "timeout" | "nice" | "command" | "exec") || word.contains('='));
        let first = words.next().unwrap_or("");
        let first = first.rsplit('/').next().unwrap_or(first);
        let rest = part.split_whitespace().collect::<Vec<_>>();
        BUILDERS.contains(&first) || first == "tee"
            || (first == "sed" && rest.iter().any(|word| word.starts_with("-i")))
            || (first == "chmod" && rest.contains(&"+x"))
            || (first == "git" && rest.contains(&"apply"))
    })
}
fn fingerprint(parts: &[&str]) -> u64 {
    let mut hash = Fx::default();
    for part in parts {
        hash.write(part.as_bytes());
        hash.write_u8(0);
    }
    hash.finish()
}

/// Compares one team's scan with what was last observed. `advanced` is the
/// claim progress the lifecycle pass saw; a raised score also counts. With no
/// earlier observation the subscription starts here and nothing is reported.
pub(super) fn fold(known: Option<&Snapshot>, scan: &Scan<'_>, advanced: bool, limits: Limits) -> (Snapshot, Found) {
    let mut ids = scan.commands.iter().map(|(_, id, ..)| *id).collect::<Vec<_>>();
    ids.sort_unstable();
    let commands = if ids.is_empty() { 0 } else { fingerprint(&ids) };
    let mut next = known.cloned().unwrap_or_default();
    let mut found = Found::default();
    // Classify only a changed retained set.
    let mut loops = Vec::new();
    if known.is_none_or(|known| known.commands != commands) {
        next.commands = commands;
        // Per member: failures per normalised command, and status-only reads.
        let mut failures: Vec<(&str, String, u32, &str)> = Vec::new();
        let mut reads: Vec<(&str, u32)> = Vec::new();
        let mut built: Vec<(&str, &str)> = Vec::new();
        for &(chat, id, command, failed) in &scan.commands {
            if crate::observe::status_style(command) {
                match reads.iter_mut().find(|(known, _)| *known == chat) {
                    Some((_, count)) => *count += 1,
                    None => reads.push((chat, 1)),
                }
            } else if builds(command) {
                built.push((id, chat));
            }
            if failed {
                let normal = normal(command);
                match failures.iter_mut().find(|(known, text, ..)| *known == chat && *text == normal) {
                    Some((_, _, count, latest)) => (*count, *latest) = (*count + 1, command),
                    None => failures.push((chat, normal, 1, command)),
                }
            }
        }
        for (chat, normal, count, command) in failures.into_iter().filter(|(.., count, _)| *count >= limits.repeat) {
            loops.push((fingerprint(&[chat, &normal]), Loop { chat: chat.to_owned(), command: excerpt(command), failures: count }));
        }
        loops.sort_by_key(|(print, _)| *print);
        let mut polling = reads.into_iter().filter(|(_, count)| *count >= limits.poll).map(|(chat, commands)| Polling { chat: chat.to_owned(), commands }).collect::<Vec<_>>();
        polling.sort_by(|a, b| a.chat.cmp(&b.chat));
        if let Some(known) = known {
            found.loops = loops.iter().filter(|(print, _)| known.loops.binary_search(print).is_err()).take(LISTED).map(|(_, found)| found.clone()).collect();
            found.polling = polling.iter().filter(|poll| known.polling.binary_search(&poll.chat).is_err()).take(LISTED).cloned().collect();
        }
        next.loops = loops.iter().map(|(print, _)| *print).collect();
        next.polling = polling.into_iter().map(|poll| poll.chat).collect();
        let mut counted = built.iter().map(|(id, _)| (*id).to_owned()).collect::<Vec<_>>();
        counted.sort_unstable();
        let fresh = known.map_or(0, |known| counted.iter().filter(|id| known.counted.binary_search(id).is_err()).count());
        let mut builders = built.iter().map(|(_, chat)| (*chat).to_owned()).collect::<Vec<_>>();
        builders.sort_unstable();
        builders.dedup();
        builders.truncate(LISTED);
        (next.counted, next.builders) = (counted, builders);
        next.churn = next.churn.saturating_add(u32::try_from(fresh).unwrap_or(u32::MAX));
    }
    let Some(known) = known else {
        // Subscription starts here: earlier history is not reported.
        (next.churn, next.churned, next.tokens, next.burned, next.score) = (0, false, scan.tokens, false, scan.score);
        return (next, found);
    };
    let progress = advanced || scan.score > known.score;
    next.score = known.score.max(scan.score);
    if progress {
        (next.churn, next.churned) = (0, false);
    } else if next.churn >= limits.churn && !next.churned {
        found.churn = Some(Churn { commands: next.churn, members: next.builders.clone() });
        next.churned = true;
    }
    if progress || scan.tokens < known.tokens {
        (next.tokens, next.burned) = (scan.tokens, false);
    } else if scan.tokens - known.tokens >= limits.burn && !known.burned {
        found.burn = Some(Burn { tokens: scan.tokens - known.tokens });
        next.burned = true;
    }
    (next, found)
}

/// The newest unix time at which any field of a probe's series rose.
pub(super) fn raised(samples: &std::collections::VecDeque<crate::scores::Sample>) -> u64 {
    let mut latest = 0;
    let mut previous: Option<&crate::scores::Sample> = None;
    for sample in samples {
        if let Some(before) = previous
            && sample.values.iter().any(|(key, value)| before.values.get(key).is_some_and(|old| value > old))
        {
            latest = latest.max(sample.unix);
        }
        previous = Some(sample);
    }
    latest
}

#[cfg(test)]
mod tests;
