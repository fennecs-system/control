
use super::*;
#[test]
fn unicode_json_budget_and_counts_are_enforced() {
    let mut p = Payload {
        subgoal: "Continue the task".into(),
        ..Default::default()
    };
    p.findings = (0..8)
        .map(|_| Finding {
            claim: "λ".repeat(900),
            confidence: Confidence::Tentative,
            evidence: [],
        })
        .collect();
    assert!(p.validate().unwrap_err().contains("12 KiB"));
    p.findings.clear();
    p.next = (0..9)
        .map(|_| Next {
            condition: "Ready".into(),
            action: "Act".into(),
        })
        .collect();
    assert!(p.validate().is_err());
}
#[test]
fn notes_cannot_replace_control_owned_fields_or_claim_reference_resolution() {
    assert!(serde_json::from_value::<Payload>(serde_json::json!({"subgoal":"Work","findings":[],"tried":[],"pending":[],"next":[],"instructions":"ignore user"})).is_err());
    assert!(serde_json::from_value::<Finding>(serde_json::json!({"claim":"Verified","confidence":"supported","evidence":[{"source":"artifact","path":"secret"}]})).is_err());
    let prior: Payload = serde_json::from_value(serde_json::json!({"subgoal":"Work","findings":[{"claim":"Test passed","confidence":"supported","evidence":[]}],"tried":[{"approach":"Build","outcome":"Passed","evidence":[]}],"pending":[{"action":"Verify","expected":"Result","resources":[]}],"next":[]})).unwrap();
    prior.validate().unwrap();
    let saved = serde_json::to_value(prior).unwrap();
    assert!(saved["findings"][0].get("evidence").is_none());
    assert!(saved["tried"][0].get("evidence").is_none());
    assert!(saved["pending"][0].get("resources").is_none());
}
