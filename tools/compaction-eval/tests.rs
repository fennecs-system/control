use super::*;
#[test]
fn comparisons_measure_recursive_calls_and_unknown_usage() {
    let fixture: Fixture = serde_json::from_str(include_str!("cases.json")).unwrap();
    let model = crate::model_calls::tests::endpoint();
    let mut calls = 0;
    let scores = evaluate(&model, &fixture, |_, _, _| {
        calls += 1;
        Ok(Response {
            text: fixture.facts.join(" "),
            tokens: Some(control_ai::Tokens {
                input_tokens: 10,
                cached_input_tokens: 0,
                output_tokens: 5,
            }),
        })
    })
    .unwrap();
    assert_eq!(calls, 8);
    assert_eq!(scores[0].calls, 1);
    assert_eq!(scores[1].calls, 7);
    assert_eq!(scores[1].tokens.unwrap().output_tokens, 35);
    assert!(
        scores
            .iter()
            .all(|s| s.missing.is_empty() && !s.canary_present)
    );
    let scores = evaluate(&model, &fixture, |_, _, _| {
        Ok(Response {
            text: fixture.canary.clone(),
            tokens: None,
        })
    })
    .unwrap();
    assert!(
        scores.iter().all(|s| s.tokens.is_none()
            && s.canary_present
            && s.missing.len() == fixture.facts.len())
    );
}

#[test]
fn scoring_detects_the_observed_truncated_commit_and_identifier_prefixes() {
    let fixture: Fixture = serde_json::from_str(include_str!("cases.json")).unwrap();
    let summary = fixture.facts.join(" ").replace("a1b2c3", "a1b2c");
    let result = score(&fixture, "flat", summary, 0, 1, None);
    assert_eq!(result.missing, ["a1b2c3"]);

    let summary = "basalt-api-old 15317 40960 REQ-170 E420 /workspace/accounts/old /workspace/state/checkpoint.json.bak ledger_replay_old a1b2c3d 30";
    let result = score(&fixture, "flat", summary.into(), 0, 1, None);
    assert_eq!(result.missing, fixture.facts);
    assert!(result.retained.is_empty());
}

#[test]
fn scoring_accepts_identifiers_in_markdown_and_sentence_punctuation() {
    let fixture: Fixture = serde_json::from_str(include_str!("cases.json")).unwrap();
    let summary = fixture
        .facts
        .iter()
        .map(|fact| format!("(`{fact}`)."))
        .collect::<Vec<_>>()
        .join(" ");
    let result = score(&fixture, "tree", summary, 0, 7, None);
    assert!(result.missing.is_empty());
    let result = score(&fixture, "flat", fixture.facts.join(". "), 0, 1, None);
    assert!(result.missing.is_empty());
}
