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
        scores
            .iter()
            .all(|s| s.tokens.is_none() && s.canary_present && s.missing.len() == 8)
    );
}
