use super::*;
use serde_json::json;

fn transcript(steps: usize, result: &str, arguments: &str) -> Vec<Value> {
    let mut messages = vec![
        json!({"role":"system","content":"s"}),
        json!({"role":"user","content":"u"}),
    ];
    for i in 0..steps {
        messages.push(json!({"role":"assistant","content":null,"tool_calls":[{"id":format!("c{i}"),"type":"function","function":{"name":"control_workspace_run","arguments":arguments}}]}));
        messages.push(json!({"role":"tool","tool_call_id":format!("c{i}"),"content":format!("{i}:{result}")}));
    }
    messages
}

#[test]
fn the_lens_keeps_recent_steps_whole_and_shrinks_older_ones() {
    let big = "line of output\n".repeat(400);
    let arguments = json!({"command":"echo setup; ".repeat(100),"timeout":30}).to_string();
    let messages = transcript(5, &big, &arguments);
    let view = Lens::default().view(&messages, 2, false);
    assert_eq!(view.len(), messages.len(), "shrinking keeps every message and pairing");
    // The two newest steps are untouched.
    assert_eq!(view[8..], messages[8..]);
    // Older ones keep a head, a marker and valid JSON arguments with every key.
    let old = view[3]["content"].as_str().unwrap();
    assert!(
        old.starts_with("0:line of output") && old.contains("more bytes of this earlier result") && old.len() < 900
    );
    let stripped: Value =
        serde_json::from_str(view[2]["tool_calls"][0]["function"]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(stripped["timeout"], 30);
    assert!(stripped["command"].as_str().unwrap().contains("bytes elided"));
    // Rendering is deterministic, so the prefix is stable from step to step.
    assert_eq!(view, Lens::default().view(&messages, 2, false));
    let shrunk = Lens::default().view(&messages, 2, true);
    assert!(
        shrunk[9]["content"]
            .as_str()
            .unwrap()
            .contains("more bytes of this earlier result")
    );
}
#[test]
fn the_verbatim_window_shrinks_older_steps_in_whole_batches() {
    let windows: Vec<usize> = (0..14).map(|count| window(count, 4, 4)).collect();
    assert_eq!(windows, [0, 1, 2, 3, 4, 5, 6, 7, 4, 5, 6, 7, 4, 5]);
    assert_eq!(
        (0..6).map(|count| window(count, 4, 1)).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4, 4],
        "batches of one: the old rolling window"
    );
    assert_eq!(window(9, usize::MAX, 4), 9);
    assert_eq!(window(9, 2, 0), 2, "a zero batch is one");
}
#[test]
fn a_result_never_repeats_a_long_argument() {
    let script = "printf 'configure the server'\n".repeat(20);
    let content = format!("$ {script}\nexit 0");
    let lean = without_repeats(content, &json!({"command":script}));
    assert!(!lean.contains("configure the server"));
    assert!(lean.contains("[the call's command argument,") && lean.ends_with("exit 0"));
    assert_eq!(without_repeats("short".into(), &json!({"command":"short"})), "short");
}
#[test]
fn a_digest_keeps_call_names_and_result_heads_within_its_bound() {
    let range = [
        json!({"role":"user","content":"Summary of steps 1–2: cloned repo"}),
        json!({"role":"assistant","content":null,"tool_calls":[{"function":{"name":"control_workspace_run","arguments":"{\"command\":\"make\"}"}}]}),
        json!({"role":"tool","content":"x".repeat(5000)}),
    ];
    let digest = digest(&range);
    assert!(digest.contains("cloned repo") && digest.contains("called control_workspace_run {\"command\":\"make\"}"));
    assert!(digest.contains("result (5000 bytes)") && digest.len() < 700);
    let many: Vec<Value> = (0..200).flat_map(|_| range.clone()).collect();
    assert!(super::digest(&many).len() <= DIGEST_BYTES + 64);
}

struct Summaries(Vec<(String, usize)>);
impl Host for Summaries {
    fn tools(&mut self) -> io::Result<Vec<Value>> {
        Ok(vec![])
    }
    fn call(&mut self, _: &str, _: &Value, _: &str) -> Value {
        Value::Null
    }
    fn poll(&mut self) -> super::super::Poll {
        super::super::Poll::Continue
    }
    fn model(&mut self, _: &Value) -> io::Result<Value> {
        Err(io::Error::other("unused"))
    }
    fn summarize(&mut self, evidence: &str, words: usize) -> io::Result<String> {
        self.0.push((evidence.to_owned(), words));
        Ok(format!("summary #{}", self.0.len()))
    }
    fn record(&mut self, _: &str, _: &str, _: &str, _: &str, _: &str) {}
    fn metered(&mut self, _: &super::super::Meter) {}
}
#[test]
fn folded_blocks_form_a_summary_tree_and_stay_folded() {
    let messages = transcript(12, &"output\n".repeat(300), "{}");
    let compactor = Compactor {
        input_bytes: 1500,
        output_tokens: 512,
    };
    let mut lens = Lens::default();
    let mut host = Summaries(Vec::new());
    // 12 steps, 4 verbatim: blocks [0,4) [4,6) [6,8), then four single steps.
    let blocks: Vec<Block> = cut(12, 4).collect();
    assert_eq!(
        blocks[4..],
        [
            Block { start: 6, level: 1 },
            Block { start: 4, level: 1 },
            Block { start: 0, level: 2 }
        ]
    );
    let oldest = lens.unfolded(12, 4).unwrap();
    assert_eq!(oldest, Block { start: 0, level: 2 });
    let done = lens.fold(&mut host, oldest, &messages, compactor).unwrap();
    // Four shrunk steps exceed the compactor's input, so the block is built
    // from its children (recursively) and then from their summaries.
    assert!(done.len() > 1, "{done:?}");
    assert_eq!(
        done.last().unwrap(),
        "Summarized steps 1–4 of this turn on the local compactor"
    );
    assert!(
        host.0
            .iter()
            .all(|(evidence, words)| evidence.len() <= 1500 && *words == crate::compaction::target_words(512))
    );
    let last = &host.0.last().unwrap().0;
    assert!(
        last.contains("Summary of steps 1–2") && last.contains("Summary of steps 3–4"),
        "{last}"
    );
    let view = lens.view(&messages, 4, false);
    assert_eq!(view.len(), 2 + 1 + 8 * 2, "one node replaces four steps");
    assert!(
        view[2]["content"]
            .as_str()
            .unwrap()
            .starts_with("Summary of steps 1–4 of this turn")
    );
    // A later step regroups blocks near the focus but keeps the fold, so the
    // prefix is unchanged; the next fold is the oldest unfolded block.
    let mut longer = messages.clone();
    longer.extend(transcript(1, "new", "{}").into_iter().skip(2));
    let next = lens.view(&longer, 4, false);
    assert_eq!(next[..3], view[..3], "the folded prefix is unchanged");
    assert_eq!(lens.unfolded(13, 4), Some(Block { start: 4, level: 1 }));
    // Folding [4,6) and [6,8) lets the cut's [0,8) block merge their summaries.
    for _ in 0..2 {
        let block = lens.unfolded(13, 5).unwrap();
        lens.fold(&mut host, block, &longer, compactor).unwrap();
    }
    let merged = Block { start: 0, level: 3 };
    let before = host.0.len();
    lens.fold(&mut host, merged, &longer, compactor).unwrap();
    assert_eq!(
        host.0.len(),
        before + 1,
        "a parent of folded children is one call over their summaries"
    );
    assert!(
        host.0
            .last()
            .unwrap()
            .0
            .lines()
            .all(|line| line.starts_with("user: Summary of steps") || !line.starts_with("user:"))
    );
    assert!(
        lens.view(&longer, 4, false)[2]["content"]
            .as_str()
            .unwrap()
            .starts_with("Summary of steps 1–8")
    );
}

#[test]
fn marked_lines_are_found_through_list_and_emphasis_markup() {
    let text = "Looking at the ELF header.\n- **Finding:** the Oracle JDK hard-codes /lib/ld-linux-aarch64.so.1\n  decision: install the glibc loader\nnot a finding: x\n### ROOT CAUSE: missing loader\nFindings are below";
    assert_eq!(
        marked(text).collect::<Vec<_>>(),
        [
            "Finding:** the Oracle JDK hard-codes /lib/ld-linux-aarch64.so.1",
            "decision: install the glibc loader",
            "ROOT CAUSE: missing loader",
        ]
    );
    let many: Vec<String> = (0..100).map(|i| format!("Finding: fact {i}")).collect();
    let kept = newest(many);
    assert!(
        kept.last().unwrap() == "Finding: fact 99" && kept.iter().map(|l| l.len() + 3).sum::<usize>() <= MARKED_BYTES
    );
}

/// A compactor that drops everything, or fails, and records retained nodes.
#[derive(Default)]
struct Forgetful {
    fail: bool,
    summaries: usize,
    retained: Vec<(Block, String, Option<[String; 2]>, usize)>,
}
impl Host for Forgetful {
    fn tools(&mut self) -> io::Result<Vec<Value>> {
        Ok(vec![])
    }
    fn call(&mut self, _: &str, _: &Value, _: &str) -> Value {
        Value::Null
    }
    fn poll(&mut self) -> super::super::Poll {
        super::super::Poll::Continue
    }
    fn model(&mut self, _: &Value) -> io::Result<Value> {
        Err(io::Error::other("unused"))
    }
    fn summarize(&mut self, _: &str, _: usize) -> io::Result<String> {
        self.summaries += 1;
        if self.fail {
            return Err(io::Error::other("Output reached the declared token limit"));
        }
        Ok("Goal & constraints: run the Java build\nFindings: none\nErrors & fixes: none\nDecisions/plan: none\nOpen questions: none\nNext step: continue".into())
    }
    fn record(&mut self, _: &str, _: &str, _: &str, _: &str, _: &str) {}
    fn metered(&mut self, _: &super::super::Meter) {}
    fn retain(
        &mut self,
        block: Block,
        text: &str,
        children: Option<[String; 2]>,
        sources: Vec<String>,
    ) -> Option<String> {
        self.retained.push((block, text.to_owned(), children, sources.len()));
        Some(format!("{:064x}", self.retained.len()))
    }
}
const FINDING: &str = "Finding: the Oracle JDK hard-codes /lib/ld-linux-aarch64.so.1, absent in the Nix image";
const DECISION: &str = "Decision: patch the interpreter with patchelf";
/// Fold a growing turn step by step, as the loop does, until it has `steps`
/// steps; returns the views after each step and the number of folds.
fn long_turn(host: &mut Forgetful, steps: usize) -> (Vec<Vec<Value>>, usize) {
    let mut messages = transcript(steps, &"output\n".repeat(300), "{}");
    messages[2]["content"] = json!(format!("Checked the ELF interpreter.\n{FINDING}"));
    let mut lens = Lens::default();
    // Marked reasoning of step 2 is never transcript, yet survives.
    lens.reasoned(1, &format!("The loader is missing, so\n{DECISION}"));
    let compactor = Compactor {
        input_bytes: 1500,
        output_tokens: 512,
    };
    let (mut views, mut folds) = (Vec::new(), 0);
    for count in 6..=steps {
        let prefix = &messages[..2 + 2 * count];
        while let Some(block) = lens.unfolded(count, 4) {
            folds += lens.fold(host, block, prefix, compactor).unwrap().len();
        }
        lens.retain_shrunk(host, prefix, count - 4);
        views.push(lens.view(prefix, 4, false));
    }
    (views, folds)
}
#[test]
fn a_marked_finding_survives_every_fold_even_when_summaries_drop_it() {
    let mut host = Forgetful::default();
    let (views, folds) = long_turn(&mut host, 24);
    assert!(folds >= 9, "{folds} folds");
    assert!(host.summaries >= 9);
    for view in &views {
        let first = view[2]["content"].as_str().unwrap();
        assert!(
            first.starts_with("Summary of steps 1–") && first.contains("Findings: none"),
            "{first}"
        );
        assert!(
            first.contains(&format!(
                "Marked by the agent in these steps (verbatim):\n- {FINDING}\n- {DECISION}"
            )),
            "{first}"
        );
    }
    // Every node joined the summary tree: a leaf per shrunk step, a node
    // over two retained children or over one original per step.
    for (block, text, children, sources) in &host.retained {
        assert!(
            children.is_some() && *sources == 0 || children.is_none() && *sources as u64 == block.len(),
            "{block:?}"
        );
        if block.level > 0 && block.start == 0 {
            assert!(text.contains(FINDING), "the retained node carries the marked lines");
        }
    }
    assert!(
        host.retained.iter().any(|(_, _, children, _)| children.is_some()),
        "parents are built over their children"
    );
    let leaves: Vec<u64> = host
        .retained
        .iter()
        .filter(|(b, ..)| b.level == 0)
        .map(|(b, ..)| b.start)
        .collect();
    let mut unique = leaves.clone();
    unique.dedup();
    assert_eq!(leaves, unique, "each step is retained once");
}
#[test]
fn the_digest_fallback_keeps_marked_findings() {
    let mut host = Forgetful {
        fail: true,
        ..Default::default()
    };
    let (views, folds) = long_turn(&mut host, 16);
    assert!(folds >= 3);
    let first = views.last().unwrap()[2]["content"].as_str().unwrap();
    assert!(first.starts_with("Digest of steps 1–"), "{first}");
    assert!(first.contains(FINDING) && first.contains(DECISION), "{first}");
}
