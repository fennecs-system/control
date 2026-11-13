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
