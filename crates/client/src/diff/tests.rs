
use super::*;
fn snapshot(files: &[(&str, &str)]) -> Snapshot {
    Snapshot {
        files: files
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect(),
        ..Snapshot::default()
    }
}
#[test]
fn replacement_has_correct_signs_counts_context_and_hunk_positions() {
    let diff = between(
        &snapshot(&[("a.rs", "one\ntwo\nthree\n")]),
        &snapshot(&[("a.rs", "one\nnew\nthree\n")]),
    );
    assert_eq!((diff.files, diff.added, diff.removed), (1, 1, 1));
    assert!(
        diff.text
            .contains("@@ -1,3 +1,3 @@\n one\n-two\n+new\n three\n")
    );
    assert_eq!(kind("+new"), Kind::Add);
    assert_eq!(kind("-two"), Kind::Remove);
}
#[test]
fn additions_deletions_empty_files_newlines_and_reverts_are_distinct() {
    let empty = snapshot(&[]);
    let created = between(&empty, &snapshot(&[("new.rs", "hello")]));
    assert!(created.text.contains("--- /dev/null\n") && created.text.contains("\\ No newline"));
    assert_eq!((created.added, created.removed), (1, 0));
    let deleted = between(&snapshot(&[("old.rs", "bye\n")]), &empty);
    assert_eq!((deleted.added, deleted.removed), (0, 1));
    assert!(deleted.text.contains("+++ /dev/null"));
    assert_eq!(between(&empty, &snapshot(&[("empty.rs", "")])).files, 1);
    assert_eq!(
        between(
            &snapshot(&[("a.rs", "same")]),
            &snapshot(&[("a.rs", "same")])
        )
        .files,
        0
    );
}
#[test]
fn unreadable_and_incomplete_snapshots_never_invent_deletions() {
    let before = snapshot(&[("a.rs", "text")]);
    let mut after = snapshot(&[]);
    after.skipped.insert("a.rs".into());
    let diff = between(&before, &after);
    assert_eq!(diff.files, 0);
    assert_eq!(diff.omitted, 1);
    after.skipped.clear();
    after.incomplete = true;
    assert_eq!(between(&before, &after).files, 0);
}
#[test]
fn huge_replacements_bound_compute_and_preview_size() {
    let a = "old\n".repeat(10000);
    let b = "new\n".repeat(10000);
    let diff = between(&snapshot(&[("big.rs", &a)]), &snapshot(&[("big.rs", &b)]));
    assert!(diff.text.len() < OUTPUT_BYTES + 1024);
    assert!(diff.omitted > 0);
    assert_eq!((diff.added, diff.removed), (10000, 10000));
}
#[test]
fn operations_reconstruct_both_inputs_for_small_repeated_line_sequences() {
    let sequences: Vec<Vec<&str>> = (0..64)
        .map(|mask| {
            (0..6)
                .map(|i| if mask & (1 << i) == 0 { "a\n" } else { "b\n" })
                .collect()
        })
        .collect();
    for a in &sequences {
        for b in &sequences {
            let ops = operations(a, b, &mut 1000);
            assert_eq!(
                ops.iter()
                    .filter(|(k, _)| *k != Kind::Add)
                    .map(|(_, s)| *s)
                    .collect::<Vec<_>>(),
                *a
            );
            assert_eq!(
                ops.iter()
                    .filter(|(k, _)| *k != Kind::Remove)
                    .map(|(_, s)| *s)
                    .collect::<Vec<_>>(),
                *b
            );
        }
    }
}
#[test]
fn file_headers_are_not_changes_and_truncated_hunks_count_only_shown_lines() {
    assert_eq!(kind("--- a/src/main.rs"), Kind::Header);
    assert_eq!(kind("+++ /dev/null"), Kind::Header);
    assert_eq!(kind("--- SQL comment removed"), Kind::Remove);
    let preview = between(
        &snapshot(&[]),
        &snapshot(&[("big.txt", &"line\n".repeat(OUTPUT_LINES + 50))]),
    );
    assert!(preview.omitted > 0);
    let shown = preview
        .text
        .lines()
        .filter(|l| kind(l) == Kind::Add)
        .count();
    assert!(
        preview.text.contains(&format!("@@ -0,0 +1,{shown} @@\n")),
        "{}",
        &preview.text[..200]
    );
}
#[test]
fn unified_hunks_pass_git_apply_check() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let root = std::env::temp_dir().join(format!("control-diff-apply-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("test.rs"), "one\ntwo\nthree\n").unwrap();
    let diff = between(
        &snapshot(&[("test.rs", "one\ntwo\nthree\n")]),
        &snapshot(&[("test.rs", "one\nnew\nthree\n")]),
    );
    let mut child = Command::new("git")
        .args(["apply", "--check"])
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(diff.text.as_bytes())
        .unwrap();
    let success = child.wait().unwrap().success();
    std::fs::remove_dir_all(root).unwrap();
    assert!(success, "Unified diff did not pass Git's parser");
}
#[test]
fn discord_preview_is_plain_bounded_and_cannot_escape_a_code_fence() {
    let preview = discord_preview("@@ -1 +1 @@\n-old\n+new\n");
    assert!(preview.starts_with("```text\n") && preview.contains("-old\n+new"));
    assert_eq!(
        discord_preview("+```injected\n"),
        "Changes available in chat."
    );
    assert!(discord_preview(&"+x\n".repeat(10000)).chars().count() < 550);
}
