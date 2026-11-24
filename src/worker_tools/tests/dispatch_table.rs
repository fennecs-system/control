//! The dispatch table against the if-chain it replaced: every name resolves
//! to the same handler family and passes the same fences in the same place.
use super::*;
use kinds::Handler;
use table::{Fence, Session};

/// Every tool name any audience or catalogue knows, plus near misses.
fn universe() -> Vec<String> {
    let mut names = crate::tool_registry::worker_definitions(true)
        .into_iter()
        .chain(crate::rooms::worker_definitions(false))
        .chain([crate::host_shell::definition()])
        .chain(crate::tool_registry::entries().iter().map(|e| e.definition.clone()))
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .chain(
            control_context::TOOLS
                .iter()
                .flat_map(|t| t.mcp_names.iter().map(|m| m.name.to_owned())),
        )
        .chain(control_context::tool_policy::POLICIES.iter().map(|p| p.name.to_owned()))
        .chain(
            [
                "control_chat",
                deferred::SEARCH,
                deferred::CALL,
                "control_agent_delegate",
                "control.browser.capture",
                "control.room.read",
                "control_web.fetch",
                "unknown",
                "",
            ]
            .map(str::to_owned),
        )
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}
/// The previous dispatcher's name tests, in its order: which fence class a
/// name passed and whether anything answered it.
fn legacy(name: &str) -> Option<Fence> {
    if name == "control_guidance" || name == deferred::SEARCH {
        return Some(Fence::Open);
    }
    if matches!(
        name,
        "control_agent_spawn_status" | "control_agent_spawn_cancel" | "control_agent_spawn" | "control_design_ask"
    ) {
        return Some(Fence::Delegation);
    }
    if name == "control_agent_spawn_blocking" {
        return Some(Fence::Standing);
    }
    let resolved = control_context::dotted_name(name)
        .filter(|dotted| {
            crate::tool_registry::lookup(dotted).is_some_and(|e| e.route == crate::tool_registry::Route::Toolbox)
        })
        .unwrap_or(name);
    let answered = crate::knowledge_tools::known(name)
        || crate::tool_registry::attempt_tools(name).is_some()
        || matches!(
            name,
            "control_github_open_pr"
                | "control_preview_start"
                | "control_preview_stop"
                | "control_project_request"
                | "control_agent_result"
                | "control_workspace_run"
        )
        || resolved == crate::model_calls::tool::NAME
        || crate::toolbox::known(resolved)
        || matches!(resolved, "control_web_fetch" | "control_browser_capture")
        || [
            "control_room_list",
            "control_room_read",
            "control_room_post",
            "control_room_watch",
            "control_room_notifications",
            "control_room_ack",
            "control_room_claim",
            "control_room_plan",
        ]
        .contains(&resolved);
    answered.then_some(Fence::Design)
}

#[test]
fn every_name_resolves_as_the_if_chain_did() {
    let not_allowed = table::lookup("control_nonexistent");
    for name in universe() {
        let entry = table::lookup(&name);
        match legacy(&name) {
            None => assert!(std::ptr::eq(entry, not_allowed), "{name} must not be answered"),
            Some(fence) => {
                assert!(!std::ptr::eq(entry, not_allowed), "{name} lost its handler");
                assert_eq!(entry.fence, fence, "{name}");
            }
        }
        // Families are found by name exactly as `attempt_tools` finds them,
        // except guidance, which the table answers first as a read.
        match (&entry.handler, crate::tool_registry::attempt_tools(&name)) {
            (Handler::Family(family), Some(expected)) => assert!(std::ptr::addr_eq(*family, expected), "{name}"),
            (Handler::Family(_), None) => panic!("{name} is not a family tool"),
            (_, Some(_)) => assert_eq!(name, "control_guidance"),
            (_, None) => {}
        }
        let unmanaged = matches!(name.as_str(), "control_room_ack" | "control_room_notifications");
        assert_eq!(entry.session == Session::Unmanaged, unmanaged, "{name}");
    }
}

/// A write persists through the checkpoint the dispatcher hands it, and a
/// rejected checkpoint leaves state as it was.
#[test]
fn a_write_handler_rolls_back_when_its_checkpoint_fails() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Work", "test").unwrap();
    state.runtime.runs[0].status = RunStatus::Running;
    let run = state.runtime.runs[0].id().to_string();
    let shared = Arc::new(Mutex::new(state));
    let envelope = json!({"nonce":"1-1","request":{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control_project_request","arguments":{"path":"/tmp/control-rollback-project"}}}});
    let call = kinds::Call::of(&envelope, None);
    let entry = table::lookup(call.name);
    assert!(matches!(entry.handler, Handler::Write(_)));
    let before = serde_json::to_value(&*shared.lock().unwrap()).unwrap();
    let state = shared.lock().unwrap();
    let caller = authority::admit(&state, &run, None).unwrap();
    let refused = entry.handler.answer(state, &shared, &caller, &call, |_| {
        Err(std::io::Error::other("disk full"))
    });
    assert_eq!(refused, control_mcp::failure("Could not save project request"));
    assert_eq!(serde_json::to_value(&*shared.lock().unwrap()).unwrap(), before);
    let state = shared.lock().unwrap();
    let caller = authority::admit(&state, &run, None).unwrap();
    let accepted = entry.handler.answer(state, &shared, &caller, &call, |_| Ok(()));
    assert_eq!(accepted["isError"], false, "{accepted}");
    assert_ne!(serde_json::to_value(&*shared.lock().unwrap()).unwrap(), before);
}
