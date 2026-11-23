//! Bounded, deterministic unified-diff previews. No subprocesses or model calls.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const ADD_RGB: (u8, u8, u8) = (181, 160, 255);
pub const REMOVE_RGB: (u8, u8, u8) = (217, 154, 171);
const OUTPUT_BYTES: usize = 32 * 1024;
const OUTPUT_LINES: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Add,
    Remove,
    Context,
    Header,
}
pub fn kind(line: &str) -> Kind {
    // File headers before hunk lines, so they are not shown as an addition/removal.
    if ["--- a/", "--- \"a/", "--- /dev/null", "+++ b/", "+++ \"b/", "+++ /dev/null"].iter().any(|p| line.starts_with(p)) {
        Kind::Header
    } else if line.starts_with('+') {
        Kind::Add
    } else if line.starts_with('-') {
        Kind::Remove
    } else if line.starts_with("diff ")
        || line.starts_with("@@")
        || line.starts_with('\\')
        || line.starts_with('…')
    {
        Kind::Header
    } else {
        Kind::Context
    }
}
#[derive(Default)]
pub struct Snapshot {
    pub files: BTreeMap<String, String>,
    pub skipped: BTreeSet<String>,
    pub incomplete: bool,
}
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct Preview {
    pub text: String,
    pub files: usize,
    pub added: usize,
    pub removed: usize,
    pub omitted: usize,
}
impl Preview {
    pub fn summary(&self) -> String {
        format!(
            "{} {} · +{} −{}{}",
            self.files,
            if self.files == 1 { "file" } else { "files" },
            self.added,
            self.removed,
            if self.omitted > 0 {
                " · preview limited"
            } else {
                ""
            }
        )
    }
}
/// Discord gets a short plain-code preview; its syntax highlighter must not
/// replace the application's palette with green additions.
pub fn discord_preview(patch: &str) -> String {
    let mut text = String::new();
    for line in patch.lines().take(12) {
        if text.chars().count() + line.chars().count() > 480 {
            break;
        }
        text.push_str(line);
        text.push('\n');
    }
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    if longest >= 3 || text.is_empty() {
        return "Changes available in chat.".into();
    }
    let fence = "```";
    format!("{fence}text\n{text}{fence}")
}
fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .collect()
}
fn path(value: &str) -> String {
    // Quote filenames so a filename cannot impersonate a hunk or terminal command.
    format!("{value:?}")
}
fn operations<'a>(old: &[&'a str], new: &[&'a str], budget: &mut usize) -> Vec<(Kind, &'a str)> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let a = &old[prefix..old.len() - suffix];
    let b = &new[prefix..new.len() - suffix];
    let mut ops: Vec<_> = old[..prefix].iter().map(|s| (Kind::Context, *s)).collect();
    let cells = (a.len() + 1).saturating_mul(b.len() + 1);
    if cells <= *budget {
        *budget -= cells;
        let width = b.len() + 1;
        let mut lcs = vec![0u32; cells];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                lcs[i * width + j] = if a[i] == b[j] {
                    1 + lcs[(i + 1) * width + j + 1]
                } else {
                    lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                ops.push((Kind::Context, a[i]));
                i += 1;
                j += 1;
            } else if i < a.len()
                && (j == b.len() || lcs[(i + 1) * width + j] >= lcs[i * width + j + 1])
            {
                ops.push((Kind::Remove, a[i]));
                i += 1;
            } else {
                ops.push((Kind::Add, b[j]));
                j += 1;
            }
        }
    } else {
        // A large changed region remains an honest replacement; cap quadratic work.
        ops.extend(a.iter().map(|s| (Kind::Remove, *s)));
        ops.extend(b.iter().map(|s| (Kind::Add, *s)));
    }
    ops.extend(
        old[old.len() - suffix..]
            .iter()
            .map(|s| (Kind::Context, *s)),
    );
    ops
}
pub fn between(before: &Snapshot, after: &Snapshot) -> Preview {
    let mut result = Preview::default();
    let names: BTreeSet<_> = before.files.keys().chain(after.files.keys()).collect();
    let mut budget = 2_000_000;
    let mut lines = 0;
    let mut limited = false;
    result.omitted = before.skipped.symmetric_difference(&after.skipped).count()
        + usize::from(before.incomplete || after.incomplete);
    for name in names {
        if before.skipped.contains(name) || after.skipped.contains(name) {
            continue;
        }
        let old = before.files.get(name);
        let new = after.files.get(name);
        if old == new {
            continue;
        }
        if (old.is_none() || new.is_none()) && (before.incomplete || after.incomplete) {
            result.omitted += 1;
            continue;
        }
        if limited || result.files >= 16 {
            result.omitted += 1;
            continue;
        }
        let a: Vec<_> = old
            .map_or("", String::as_str)
            .split_inclusive('\n')
            .collect();
        let b: Vec<_> = new
            .map_or("", String::as_str)
            .split_inclusive('\n')
            .collect();
        let ops = operations(&a, &b, &mut budget);
        result.added += ops.iter().filter(|(k, _)| *k == Kind::Add).count();
        result.removed += ops.iter().filter(|(k, _)| *k == Kind::Remove).count();
        result.files += 1;
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for (i, (kind, _)) in ops
            .iter()
            .enumerate()
            .filter(|(_, (k, _))| *k != Kind::Context)
        {
            let _ = kind;
            let start = i.saturating_sub(3);
            let end = (i + 4).min(ops.len());
            if let Some(last) = ranges.last_mut().filter(|last| last.1 >= start) {
                last.1 = end;
            } else {
                ranges.push((start, end));
            }
        }
        let mut patch = format!(
            "diff --git {} {}\n--- {}\n+++ {}\n",
            path(&format!("a/{name}")),
            path(&format!("b/{name}")),
            if old.is_some() {
                path(&format!("a/{name}"))
            } else {
                "/dev/null".into()
            },
            if new.is_some() {
                path(&format!("b/{name}"))
            } else {
                "/dev/null".into()
            }
        );
        if patch.len() + result.text.len() > OUTPUT_BYTES {
            result.omitted += 1;
            limited = true;
            continue;
        }
        let (mut previous, mut old_line, mut new_line) = (0, 0, 0);
        for (start, end) in ranges {
            for (kind, _) in &ops[previous..start] {
                old_line += usize::from(*kind != Kind::Add);
                new_line += usize::from(*kind != Kind::Remove);
            }
            let old_count = ops[start..end]
                .iter()
                .filter(|(k, _)| *k != Kind::Add)
                .count();
            let new_count = ops[start..end]
                .iter()
                .filter(|(k, _)| *k != Kind::Remove)
                .count();
            let header = |old_count: usize, new_count: usize| format!(
                "@@ -{},{} +{},{} @@\n",
                old_line + usize::from(old_count > 0),
                old_count,
                new_line + usize::from(new_count > 0),
                new_count
            );
            let header_at = patch.len();
            patch.push_str(&header(old_count, new_count));
            let (mut shown_old, mut shown_new) = (0, 0);
            for (kind, text) in &ops[start..end] {
                if patch.len() + result.text.len() >= OUTPUT_BYTES || lines >= OUTPUT_LINES {
                    limited = true;
                    break;
                }
                let prefix = match kind {
                    Kind::Add => '+',
                    Kind::Remove => '-',
                    _ => ' ',
                };
                let text_clean = clean(text.trim_end_matches('\n'));
                let remaining = OUTPUT_BYTES.saturating_sub(result.text.len() + patch.len() + 2);
                if text_clean.len() > remaining {
                    limited = true;
                    break;
                }
                patch.push(prefix);
                patch.push_str(&text_clean);
                patch.push('\n');
                lines += 1;
                shown_old += usize::from(*kind != Kind::Add);
                shown_new += usize::from(*kind != Kind::Remove);
                if !text.ends_with('\n') {
                    patch.push_str("\\ No newline at end of file\n");
                }
            }
            if limited {
                // A truncated hunk's header must describe only the lines shown.
                let end = header_at + header(old_count, new_count).len();
                patch.replace_range(header_at..end, &header(shown_old, shown_new));
                break;
            }
            for (kind, _) in &ops[start..end] {
                old_line += usize::from(*kind != Kind::Add);
                new_line += usize::from(*kind != Kind::Remove);
            }
            previous = end;
        }
        result.text.push_str(&patch);
        if limited {
            result.omitted += 1;
        }
    }
    if result.omitted > 0 {
        result
            .text
            .push_str("… Preview limited; binary, oversized or unreadable files may be omitted.\n");
    }
    result
}

#[cfg(test)]
mod tests;
