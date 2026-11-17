use super::*;
#[test]
fn scoped_paths_reach_original_evidence_and_page_it_without_model_calls() {
    let (mut state, job) = super::super::tests::fixture();
    let dir = super::super::tests::scratch();
    let view = super::super::execute(&job, &dir, |_, _, _| {
        Ok(control_ai::model::Response {
            text: "Retained constraint".into(),
            tokens: None,
        })
    })
    .unwrap();
    let root = view
        .nodes
        .iter()
        .max_by_key(|n| n.level)
        .unwrap()
        .key
        .clone();
    state
        .runtime
        .compaction
        .views
        .insert(job.chat.clone(), view);
    let mut read = Read {
        root: Some(root.clone()),
        limit_bytes: 17,
        ..Default::default()
    };
    loop {
        let value = execute(prepare(&state, &job.chat, read.clone()).unwrap(), &dir).unwrap();
        assert!(value["text"].as_str().unwrap().len() <= 17);
        if value["children"].as_array().unwrap().is_empty() {
            break;
        }
        read.path.push(0);
    }
    read.source = true;
    let mut original = String::new();
    loop {
        let value = execute(prepare(&state, &job.chat, read.clone()).unwrap(), &dir).unwrap();
        original.push_str(value["text"].as_str().unwrap());
        if let Some(offset) = value["next_offset"].as_u64() {
            read.offset = offset as usize;
        } else {
            break;
        }
    }
    assert!(original.contains("original task"));
    assert!(original.contains("svc-a"));
    let other = crate::chat::create(&mut state);
    assert!(prepare(&state, &other, read.clone()).is_err());
    read.path.push(2);
    assert!(prepare(&state, &job.chat, read).is_err());
    // Editing even valid JSON is detected by the content checksum.
    let mut node = load(&dir, &root).unwrap();
    node.response.text = "corrupted factual claim".into();
    crate::config::write_in(
        &dir,
        &format!("{root}.json"),
        &serde_json::to_vec(&node).unwrap(),
    )
    .unwrap();
    assert!(
        execute(
            prepare(
                &state,
                &job.chat,
                Read {
                    root: Some(root),
                    ..Default::default()
                }
            )
            .unwrap(),
            &dir
        )
        .is_err()
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn recall_serves_operator_revisions_and_redacts_dropped_source() {
    let (mut state, job) = super::super::tests::fixture();
    let dir = super::super::tests::scratch();
    let view = super::super::execute(&job, &dir, |_, _, _| {
        Ok(control_ai::model::Response { text: "Automatic".into(), tokens: None })
    })
    .unwrap();
    let root = view.nodes.iter().find(|n| n.start == 0 && n.level == 4).unwrap().key.clone();
    state.runtime.compaction.views.insert(job.chat.clone(), view);
    state.chats[0].memory = Some(Box::new(super::super::Edits {
        revisions: vec![super::super::surgery::Revision {
            id: 1, start: 0, level: 4, emphasis: "castle layout".into(), text: "Operator revision".into(),
            model: "local-qwen".into(), tokens: None, base: root.clone(), created_unix: 1,
        }],
        selected: vec![super::super::surgery::Selection { start: 0, level: 4, revision: 1 }],
        drops: vec![super::super::surgery::Redaction { id: 1, start: 1, end: 2, created_unix: 1, restored_unix: None }],
        ..Default::default()
    }));
    let read = |path: Vec<usize>, source: bool| {
        execute(prepare(&state, &job.chat, Read { root: Some(root.clone()), path, source, limit_bytes: 16384, ..Default::default() }).unwrap(), &dir).unwrap()
    };
    // The revision overlaps the dropped entry, so recall serves the automatic text.
    assert_eq!(read(Vec::new(), false)["text"], "Automatic");
    state.chats[0].memory.as_mut().unwrap().drops[0].restored_unix = Some(2);
    let read = |path: Vec<usize>, source: bool| {
        execute(prepare(&state, &job.chat, Read { root: Some(root.clone()), path, source, limit_bytes: 16384, ..Default::default() }).unwrap(), &dir).unwrap()
    };
    let summary = read(Vec::new(), false);
    assert_eq!(summary["text"], "Operator revision");
    assert_eq!(summary["operator_revision"]["emphasis"], "castle layout");
    assert_eq!(read(vec![0], false)["text"], "Automatic", "unrevised children are unchanged");
    state.chats[0].memory.as_mut().unwrap().drops[0].restored_unix = None;
    let source = execute(prepare(&state, &job.chat, Read { root: Some(root.clone()), path: vec![0, 0], source: true, limit_bytes: 16384, ..Default::default() }).unwrap(), &dir).unwrap();
    let text = source["text"].as_str().unwrap();
    assert!(text.contains("Entry 0:") && !text.contains("Entry 1:") && text.contains("[Entry 2 was removed"), "{text}");
    std::fs::remove_dir_all(dir).unwrap();
}
