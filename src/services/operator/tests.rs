use super::*;
use crate::services::tests::{admit_fixture, fixture};

#[test]
fn operator_lists_and_stops_only_the_chat_or_team_services() {
    let (mut state, run, lead, time) = fixture();
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    let service = state.services[0].resource.id.as_str().to_owned();
    // A spawned child of the lead and an unrelated chat.
    let child = crate::chat::create(&mut state);
    state.chats.last_mut().unwrap().parent_chat = Some(lead.clone());
    let other = crate::chat::create(&mut state);

    assert_eq!(
        list(&state.services, &state.chats, &lead, false).unwrap()["services"][0]["service_id"],
        service
    );
    assert_eq!(
        list(&state.services, &state.chats, &lead, false).unwrap()["services"][0]["chat"],
        lead
    );
    assert!(
        list(&state.services, &state.chats, &child, false).unwrap()["services"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        list(&state.services, &state.chats, &other, true).unwrap()["services"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(list(&state.services, &state.chats, "c-missing", false).is_err());

    // Another chat's stop never reaches this one; a named unknown service fails.
    assert_eq!(
        stop(&mut state, &other, true, None, |_| Ok(())).unwrap()["stopped"],
        json!([])
    );
    assert!(stop(&mut state, &lead, false, Some("svc-missing"), |_| Ok(())).is_err());
    assert!(matches!(state.services[0].resource.phase, Phase::Requested));

    // A failed save leaves the service unchanged.
    assert!(stop(&mut state, &lead, true, None, |_| Err(fail("disk full"))).is_err());
    assert!(matches!(state.services[0].resource.phase, Phase::Requested));

    // The lead's team includes its own services.
    let stopped = stop(&mut state, &lead, true, None, |_| Ok(())).unwrap();
    assert_eq!(stopped["stopped"], json!([service]));
    assert!(!matches!(state.services[0].resource.phase, Phase::Requested));
}
