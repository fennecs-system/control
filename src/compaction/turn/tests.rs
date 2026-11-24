use super::*;

/// A running attempt in a fresh chat: (state, chat, run, generation).
fn running() -> (Store, String, String, String) {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Set up the Java build", "test").unwrap();
    crate::worker::checkpoint_next(&mut state, |_| Ok(())).unwrap();
    let run = &state.runtime.runs[0];
    let generation = run.attempts.last().unwrap().workspace_id.clone().unwrap();
    let id = run.id().as_str().to_owned();
    (crate::store::Store::new(state), chat, id, generation)
}
const DIAGNOSIS: &str = "tool: $ readelf -l /opt/jdk/bin/java\n[Requesting program interpreter: /lib/ld-linux-aarch64.so.1]\n$ ls /lib/ld-linux-aarch64.so.1\nls: cannot access '/lib/ld-linux-aarch64.so.1': No such file or directory";

#[test]
fn compacted_steps_join_the_tree_and_recall_returns_their_verbatim_excerpts() {
    let (shared, chat, run, generation) = running();
    let model = crate::model_calls::tests::endpoint();
    let step = |n: usize| {
        format!(
            "assistant: step {n}\n[call control_workspace_run {{\"command\":\"make {n}\"}}]\ntool: {}",
            "build output\n".repeat(40)
        )
    };
    // Steps 1–2 leave the verbatim window as shrunk leaves; step 2 holds the diagnosis.
    let leaf = |n: u64, source: String| {
        retain(
            &shared,
            &run,
            &generation,
            &model,
            Block { start: n, level: 0 },
            "shrunk",
            None,
            vec![source],
        )
    };
    let first = leaf(0, step(1)).unwrap();
    let second = leaf(1, format!("{}\n{DIAGNOSIS}", step(2))).unwrap();
    {
        let state = shared.guard().unwrap();
        let roots: Vec<_> = state.runtime.compaction.turns[0]
            .roots
            .iter()
            .map(|r| r.key.clone())
            .collect();
        assert_eq!(roots, [first.clone(), second.clone()]);
    }
    // Folding them from their children makes one root over both, in the
    // same node shape as a chat summary: two children, no sources.
    let parent = retain(
        &shared,
        &run,
        &generation,
        &model,
        Block { start: 0, level: 1 },
        "Findings:\n- step 2 found the JDK's interpreter missing",
        Some([first.clone(), second.clone()]),
        Vec::new(),
    )
    .unwrap();
    // A malformed leaf is refused.
    assert!(
        retain(
            &shared,
            &run,
            &generation,
            &model,
            Block { start: 2, level: 1 },
            "x",
            None,
            vec!["one".into()]
        )
        .is_err()
    );
    let state = shared.guard().unwrap();
    let actor = state.chats[0].actor.clone();
    let listing = listing(&state.runtime.compaction, &chat, &actor);
    assert_eq!(listing[0]["roots"].as_array().unwrap().len(), 1, "{listing}");
    assert_eq!(listing[0]["roots"][0]["key"], parent.as_str());
    assert_eq!(listing[0]["attempt"], generation.as_str());
    assert!(
        locate(&state.runtime.compaction, &chat, &actor, &parent).is_some()
            && locate(&state.runtime.compaction, &chat, &actor, &first).is_none()
    );
    // Another conversation sees none of it.
    assert!(locate(&state.runtime.compaction, "other", &actor, &parent).is_none());
    assert!(plan(&state.runtime.compaction, &chat, &actor, " ", 4096).is_err());
    drop(state);
    // The agent's own tool: list, search, then read the hit verbatim.
    use crate::tool_registry::{AttemptCall, AttemptRef, AttemptTools, LocalHost};
    let host = LocalHost(&shared);
    let cx = AttemptCall {
        host: &host,
        attempt: AttemptRef {
            run: &run,
            generation: &generation,
            nonce: "recall",
        },
    };
    let read = |args: Value| super::super::Tools.call(&cx, "control_history_read", &args);
    let listed = read(json!({})).unwrap();
    assert_eq!(listed["turns"][0]["roots"][0]["key"], parent.as_str(), "{listed}");
    let found = read(json!({"query":"LD-LINUX-aarch64 cannot"})).unwrap();
    let hits = found["hits"].as_array().unwrap();
    let hit = hits
        .iter()
        .find(|h| h["kind"] == "source")
        .unwrap_or_else(|| panic!("{found}"));
    assert_eq!(
        (hit["step"].as_u64(), &hit["root"], &hit["path"]),
        (Some(2), &json!(parent), &json!([1]))
    );
    let excerpt = hit["excerpt"].as_str().unwrap();
    assert!(excerpt.contains("cannot access '/lib/ld-linux-aarch64.so.1'"), "{hit}");
    // The hit's root, path and offset read the same original text.
    let page = read(json!({"root":parent,"path":[1],"source":true,"offset":hit["offset"],"limit_bytes":excerpt.len()}))
        .unwrap();
    assert_eq!(page["text"], excerpt);
    assert_eq!(
        read(json!({"root":parent})).unwrap()["text"],
        "Findings:\n- step 2 found the JDK's interpreter missing"
    );
    // A term nowhere in the steps finds nothing; bad queries are refused.
    let none = read(json!({"query":"wrong-arch"})).unwrap();
    assert!(
        none["hits"].as_array().unwrap().is_empty() && none["complete"] == true,
        "{none}"
    );
    assert!(read(json!({"query":"x".repeat(201)})).is_err());
    assert!(read(json!({"query":"jdk","limit_bytes":0})).is_err());
    // The operator sees the same trees on the DM's Memory Tree page, and the
    // evidence reader expands them down to the original step.
    let page = super::super::render(&shared.guard().unwrap(), &chat, "").unwrap();
    assert!(
        page.contains("Local turn step trees")
            && page.contains(&format!("?root={parent}'>Local turn {generation}: steps 1–2")),
        "{page}"
    );
    let get = |query: &str| {
        let mut out = Vec::new();
        assert!(super::super::recall::http(
            &mut out,
            &shared,
            "GET",
            &format!("/dm/{chat}/compaction/read"),
            query
        ));
        String::from_utf8(out).unwrap()
    };
    assert!(get("").contains(&format!("?root={parent}'>Local turn")));
    let step = get(&format!("root={parent}&path=1&source=true"));
    assert!(step.contains("200 OK") && step.contains("cannot access"), "{step}");
}

#[test]
fn every_tree_node_uses_one_structured_summary_format() {
    // Chat blocks and a local turn's step blocks are summarized by the same
    // prompt, so a parent merges its children's sections.
    for section in [
        "Goal & constraints",
        "Findings",
        "Errors & fixes",
        "Decisions/plan",
        "Open questions",
        "Next step",
        "Merge earlier summaries",
        "Finding:",
    ] {
        assert!(SYSTEM.contains(section), "{section}");
    }
    assert!(system_within(100).len() <= SYSTEM.len() + TARGET_BYTES);
    assert!(SYSTEM.len() < 640, "small compactors keep room for evidence");
}

#[test]
fn only_recent_attempts_stay_recallable() {
    let (shared, _, run, generation) = running();
    let model = crate::model_calls::tests::endpoint();
    let leaf = |shared: &Store, generation: &str| {
        retain(
            shared,
            &run,
            generation,
            &model,
            Block { start: 0, level: 0 },
            "shrunk",
            None,
            vec!["original".into()],
        )
    };
    leaf(&shared, &generation).unwrap();
    let first = directory(&generation).unwrap();
    assert!(first.is_dir());
    // Later attempts of the run, each the current one when it retains.
    for attempt in 0..TURNS {
        let next = format!("{generation}-later-{attempt}");
        shared.guard().unwrap().runtime.runs[0]
            .attempts
            .last_mut()
            .unwrap()
            .workspace_id = Some(next.clone());
        leaf(&shared, &next).unwrap();
    }
    let state = shared.guard().unwrap();
    assert_eq!(state.runtime.compaction.turns.len(), TURNS);
    assert!(
        !state
            .runtime
            .compaction
            .turns
            .iter()
            .any(|t| t.generation == generation)
    );
    assert!(!first.exists(), "an evicted attempt's step tree is deleted");
    drop(state);
    // Only the current attempt may add to its tree, and paths stay inside.
    assert!(leaf(&shared, &generation).is_err());
    assert!(directory("../escape").is_err());
}
