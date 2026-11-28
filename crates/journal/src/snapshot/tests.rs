use super::*;
use serde_json::json;

fn replay(old: &Value, patch: &[u8]) -> Value {
    let mut document = old.clone();
    apply(&mut document, edits(patch).unwrap()).unwrap();
    document
}
fn check(old: &Value, new: &Value) -> usize {
    let (a, b) = (encode(old).unwrap(), encode(new).unwrap());
    assert_eq!(
        a.bytes(),
        serde_json::to_vec(old).unwrap(),
        "encode writes compact JSON"
    );
    match diff(&a, &b) {
        None => {
            assert_eq!(old, new);
            0
        }
        Some(patch) => {
            assert_eq!(&replay(old, &patch), new, "patch {}", String::from_utf8_lossy(&patch));
            patch.len()
        }
    }
}
fn message(i: usize) -> Value {
    json!({"role": "assistant", "id": i, "text": format!("message {i} ").repeat(20)})
}
fn state(chats: usize, messages: usize) -> Value {
    json!({
        "chats": (0..chats).map(|c| json!({"id": format!("chat-{c}"), "revision": 1, "messages": (0..messages).map(message).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "runtime": {"runs": (0..50).map(|r| json!({"id": format!("run-{r}"), "status": "completed", "result": "done ".repeat(60)})).collect::<Vec<_>>()},
        "watches": [],
        "note": "",
    })
}

#[test]
fn identical_documents_need_no_patch() {
    let document = state(3, 10);
    assert_eq!(check(&document, &document.clone()), 0);
}

/// What a value written from saved bytes looks like: its range without members.
fn forget_members(encoded: &mut Encoded, node: usize) {
    let end = encoded.nodes[node].subtree_end as usize;
    let removed = (end - node - 1) as u32;
    encoded.nodes.drain(node + 1..end);
    for other in &mut encoded.nodes {
        if other.subtree_end as usize >= end {
            other.subtree_end -= removed;
        }
    }
    encoded.nodes[node].subtree_end = node as u32 + 1;
}

#[test]
fn values_saved_whole_are_still_diffed_inside() {
    let old = state(6, 40);
    let mut new = old.clone();
    new["chats"][2]["messages"].as_array_mut().unwrap().push(message(40));
    let mut base = encode(&old).unwrap();
    // The third chat as cached bytes: find its node and drop its members.
    let chats = base
        .children(0)
        .into_iter()
        .find(|&c| base.key(c) == b"\"chats\"")
        .unwrap();
    let third = base.children(chats)[2];
    forget_members(&mut base, third);
    let mut current = encode(&new).unwrap();
    let patch = diff(&base, &current).unwrap();
    assert_eq!(replay(&old, &patch), new);
    assert!(patch.len() < 400, "{} bytes: the message, not the chat", patch.len());
    // And the other way round: the new side saved whole.
    let chats = current
        .children(0)
        .into_iter()
        .find(|&c| current.key(c) == b"\"chats\"")
        .unwrap();
    let third = current.children(chats)[2];
    forget_members(&mut current, third);
    let patch = diff(&encode(&old).unwrap(), &current).unwrap();
    assert_eq!(replay(&old, &patch), new);
    assert!(patch.len() < 400);
    // Both sides saved whole.
    let patch = diff(&base, &current).unwrap();
    assert_eq!(replay(&old, &patch), new);
    assert!(patch.len() < 400);
}

/// Removing scattered elements (runs leaving for the archive) costs the
/// removals, not a rewrite of every element that shifted.
#[test]
fn scattered_removals_cost_the_removals() {
    let old = state(1, 0);
    let mut new = old.clone();
    let runs = new["runtime"]["runs"].as_array_mut().unwrap();
    let mut index = 0;
    runs.retain(|_| {
        index += 1;
        index % 5 != 0
    });
    runs[3]["status"] = json!("changed");
    runs.push(json!({"id": "appended"}));
    let size = check(&old, &new);
    assert!(size < 1200, "{size} bytes for ten removals, a change and an append");
    check(&new, &old);
}

#[test]
fn a_new_message_costs_the_message_not_the_chat() {
    let old = state(6, 400);
    let mut new = old.clone();
    new["chats"][3]["messages"].as_array_mut().unwrap().push(message(400));
    new["chats"][3]["revision"] = json!(2);
    let size = check(&old, &new);
    let whole = serde_json::to_vec(&new).unwrap().len();
    assert!(size < 400, "{size} bytes for one message out of {whole}");
}

#[test]
fn members_and_elements_are_added_changed_and_removed() {
    let old = state(4, 5);
    let mut new = old.clone();
    new["runtime"]["runs"][7]["status"] = json!("interrupted");
    new["runtime"]["runs"].as_array_mut().unwrap().remove(20);
    new["runtime"]["runs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "run-new", "status": "queued"}));
    new["runtime"]["notice"] = json!("hello");
    new.as_object_mut().unwrap().remove("note");
    new["chats"].as_array_mut().unwrap().insert(0, json!({"id": "first"}));
    new["watches"] = json!([{"id": "w"}]);
    check(&old, &new);
    check(&new, &old);
    check(&old, &json!({"replaced": true}));
    check(&json!([1, 2, 3]), &json!({"a": 1}));
}

/// Deterministic pseudo-random edits; every patch must replay exactly.
#[test]
fn random_edit_sequences_replay_to_the_same_document() {
    let mut seed = 0x9e3779b97f4a7c15u64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n.max(1) as u64) as usize
    };
    let mut current = state(5, 30);
    for step in 0..400 {
        let mut new = current.clone();
        for _ in 0..1 + next(3) {
            let chats = new["chats"].as_array_mut().unwrap();
            let chat = next(chats.len());
            match next(7) {
                0 => chats[chat]["messages"].as_array_mut().unwrap().push(message(step)),
                1 => {
                    let messages = chats[chat]["messages"].as_array_mut().unwrap();
                    if !messages.is_empty() {
                        let at = next(messages.len());
                        messages.remove(at);
                    }
                }
                2 => chats[chat]["revision"] = json!(step),
                3 => chats.push(json!({"id": format!("chat-new-{step}"), "messages": []})),
                4 if chats.len() > 1 => {
                    chats.remove(chat);
                }
                5 => {
                    let runs = new["runtime"]["runs"].as_array_mut().unwrap();
                    let run = next(runs.len());
                    runs[run]["status"] = json!(format!("s{step}"));
                    runs[run][format!("k{}", next(4))] = json!(step);
                }
                _ => new["runtime"]["counter"] = json!(step),
            }
        }
        check(&current, &new);
        current = new;
    }
}

/// Indexing bytes finds exactly the ranges encoding records, for any document.
#[test]
fn indexing_bytes_matches_encoding() {
    let documents = [
        json!(null),
        json!("a \"quoted\" \\ string with ] and } inside"),
        json!([1, -2.5e10, true, null, "x", [], {}]),
        json!({"k\"ey": {"nested": [{"a": "x".repeat(300)}, {"b": [1, 2, 3]}]}, "": ""}),
        state(4, 20),
    ];
    for document in documents {
        let encoded = encode(&document).unwrap();
        assert_eq!(index(encoded.bytes().to_vec()).unwrap(), encoded, "{document}");
    }
    assert!(
        index(b"{\"a\":".to_vec()).is_err() && index(b"[1,2".to_vec()).is_err() && index(b"{} x".to_vec()).is_err()
    );
}

#[test]
fn snapshots_carry_their_name_and_schema_and_are_recognised() {
    let schema = json!({"version": 1, "lenses": [[]]});
    for document in [json!({}), state(1, 2)] {
        let encoded = encode(&document).unwrap();
        let bytes = snapshot(encoded.bytes(), "n1", &schema);
        assert_eq!(
            base(&bytes, "n1", &schema),
            Some(encoded.clone()),
            "the file is its own diff base"
        );
        assert_eq!(base(&bytes, "n2", &schema), None);
        let mut parsed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(identify(&mut parsed), (Some("n1".into()), Some(schema.clone())));
        assert_eq!(parsed, document);
    }
}

#[test]
fn a_log_replays_only_its_own_snapshot_and_stops_at_a_bad_line() {
    let line = |s: &str| s.as_bytes().to_vec();
    let lines = vec![
        line(r#"{"snapshot":"a","schema":{"version":0}}"#),
        commit(br#"[["s",["x"],1]]"#),
        line("[[\"s\",[\"x\"],"),
        commit(br#"[["s",["x"],3]]"#),
    ];
    let (_, valid) = patches(lines.clone(), "a").unwrap();
    assert_eq!(valid.len(), 1, "a corrupt line ends the valid prefix");
    assert!(patches(lines, "b").is_none());
    assert!(patches(Vec::new(), "a").is_none());
    assert_eq!(complete_lines(b"{}\n[1]\n[2"), vec![b"{}".to_vec(), b"[1]".to_vec()]);
}
#[test]
fn readers_see_snapshot_plus_log_without_owning_either() {
    let dir = std::env::temp_dir().join(format!("control-snapshot-read-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (snap, log) = (dir.join("state.json"), dir.join("state.log"));
    let schema = Schema::default();
    let read_file = |p: &Path| std::fs::read(p);
    assert_eq!(read(&snap, &log, &schema, read_file).unwrap(), None);
    let old = state(2, 3);
    let mut new = old.clone();
    new["chats"][1]["messages"].as_array_mut().unwrap().push(message(9));
    let (a, b) = (encode(&old).unwrap(), encode(&new).unwrap());
    let schema_value = serde_json::to_value(&schema).unwrap();
    std::fs::write(&snap, snapshot(a.bytes(), "n", &schema_value)).unwrap();
    let mut text = header("n", &schema_value);
    text.push(b'\n');
    text.extend(commit(&diff(&a, &b).unwrap()));
    text.extend(b"\n[[\"s\",[\"torn");
    std::fs::write(&log, &text).unwrap();
    assert_eq!(read(&snap, &log, &schema, read_file).unwrap(), Some(new));
    // A log of another snapshot is obsolete: the snapshot already holds it.
    std::fs::write(&snap, snapshot(a.bytes(), "m", &schema_value)).unwrap();
    assert_eq!(read(&snap, &log, &schema, read_file).unwrap(), Some(old));
    std::fs::remove_dir_all(&dir).unwrap();
}
