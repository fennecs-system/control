use super::*;
use serde_json::json;

fn transcript(steps: usize, result: &str, arguments: &str) -> Vec<Value> {
    let mut messages = vec![json!({"role":"system","content":"s"}), json!({"role":"user","content":"u"})];
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
    assert!(old.starts_with("0:line of output") && old.contains("more bytes of this earlier result") && old.len() < 900);
    let stripped: Value = serde_json::from_str(view[2]["tool_calls"][0]["function"]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(stripped["timeout"], 30);
    assert!(stripped["command"].as_str().unwrap().contains("bytes elided"));
    // Rendering is deterministic, so the prefix is stable from step to step.
    assert_eq!(view, Lens::default().view(&messages, 2, false));
    let shrunk = Lens::default().view(&messages, 2, true);
    assert!(shrunk[9]["content"].as_str().unwrap().contains("more bytes of this earlier result"));
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
    fn tools(&mut self) -> io::Result<Vec<Value>> { Ok(vec![]) }
    fn call(&mut self, _: &str, _: &Value, _: &str) -> Value { Value::Null }
    fn poll(&mut self) -> super::super::Poll { super::super::Poll::Continue }
    fn model(&mut self, _: &Value) -> io::Result<Value> { Err(io::Error::other("unused")) }
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
    let compactor = Compactor { input_bytes: 1500, output_tokens: 512 };
    let mut lens = Lens::default();
    let mut host = Summaries(Vec::new());
    // 12 steps, 4 verbatim: blocks [0,4) [4,6) [6,8), then four single steps.
    let blocks: Vec<Block> = cut(12, 4).collect();
    assert_eq!(blocks[4..], [Block { start: 6, level: 1 }, Block { start: 4, level: 1 }, Block { start: 0, level: 2 }]);
    let oldest = lens.unfolded(12, 4).unwrap();
    assert_eq!(oldest, Block { start: 0, level: 2 });
    let done = lens.fold(&mut host, oldest, &messages, compactor).unwrap();
    // Four shrunk steps exceed the compactor's input, so the block is built
    // from its children (recursively) and then from their summaries.
    assert!(done.len() > 1, "{done:?}");
    assert_eq!(done.last().unwrap(), "Summarized steps 1–4 of this turn on the local compactor");
    assert!(host.0.iter().all(|(evidence, words)| evidence.len() <= 1500 && *words == crate::compaction::target_words(512)));
    let last = &host.0.last().unwrap().0;
    assert!(last.contains("Summary of steps 1–2") && last.contains("Summary of steps 3–4"), "{last}");
    let view = lens.view(&messages, 4, false);
    assert_eq!(view.len(), 2 + 1 + 8 * 2, "one node replaces four steps");
    assert!(view[2]["content"].as_str().unwrap().starts_with("Summary of steps 1–4 of this turn"));
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
    assert_eq!(host.0.len(), before + 1, "a parent of folded children is one call over their summaries");
    assert!(host.0.last().unwrap().0.lines().all(|line| line.starts_with("user: Summary of steps") || !line.starts_with("user:")));
    assert!(lens.view(&longer, 4, false)[2]["content"].as_str().unwrap().starts_with("Summary of steps 1–8"));
}
