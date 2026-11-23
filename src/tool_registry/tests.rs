use super::*;
#[test]
fn schemas_and_transport_policy_share_one_registration() {
    let mut names = std::collections::HashSet::new();
    for entry in entries() {
        assert!(names.insert(entry.name()));
        assert!(entry.definition["inputSchema"].is_object());
        if entry.checkpoint {
            assert_eq!(entry.definition["annotations"]["readOnlyHint"], false);
        }
        if entry.bridge == Bridge::Read {
            assert!(!entry.checkpoint);
            assert_ne!(entry.definition["annotations"]["readOnlyHint"], false);
        }
        assert!(
            control_context::TOOLS
                .iter()
                .any(|t| t.mcp_names.iter().any(|m| m.name == entry.name())),
            "Missing UI registration: {}",
            entry.name()
        );
    }
    assert_eq!(names.len(), control_context::tool_policy::POLICIES.len());
    for policy in control_context::tool_policy::POLICIES {
        assert!(names.contains(policy.name));
    }
    for name in [
        "control.chat.create",
        "control.chat.send",
        "control.room.post",
        "control.chat.lenses",
        "control.project.request",
        "control_github_open_pr",
    ] {
        assert!(checkpoint(name));
    }
    for name in ["control.chat.send", "unknown", "control.browser.capture"] {
        assert!(!bridge_forward(name));
    }
    assert!(bridge_forward("control.project.request"));
    assert!(
        !http_definitions()
            .iter()
            .any(|d| d["name"] == "control.browser.capture")
    );
}
#[test]
fn every_worker_schema_is_registered_for_ui_and_provider_discovery() {
    let names = control_context::worker_mcp_names();
    for definition in worker_definitions(true) {
        let name = definition["name"].as_str().unwrap();
        assert!(
            control_context::tool_for_mcp(name).is_some(),
            "Missing UI registration: {name}"
        );
        let worker_name = name.replace('.', "_");
        assert!(
            names.contains(&worker_name.as_str()),
            "Missing provider discovery: {name}"
        );
    }
}
#[test]
fn attempt_tools_route_each_name_to_exactly_one_group() {
    let workers = worker_definitions(true);
    assert!(!worker_definitions(false).iter().any(|tool| matches!(
        tool["name"].as_str(),
        Some("control_github_read" | "control_github_write")
    )));
    assert!(!control_context::tool_policy::after_handoff("control_github_write"));
    let mut seen = std::collections::HashSet::new();
    for &tools in &ATTEMPT_TOOLS {
        let definitions = tools.definitions();
        // The agent chat tool is listed only when the operator enables it.
        assert!(!definitions.is_empty() || tools.known("control_chat"));
        for definition in definitions {
            let name = definition["name"].as_str().unwrap();
            assert_eq!(
                ATTEMPT_TOOLS.iter().filter(|other| other.known(name)).count(),
                1,
                "{name}"
            );
            assert!(std::ptr::addr_eq(attempt_tools(name).unwrap(), tools), "{name}");
            assert!(workers.iter().any(|worker| worker["name"] == name), "{name}");
            assert!(seen.insert(name.to_owned()), "{name}");
        }
    }
    for name in [
        "control_agent_result",
        "control_workspace_run",
        "control.chat.send",
        "unknown",
        "",
    ] {
        assert!(attempt_tools(name).is_none(), "{name}");
    }
}
/// Every tool schema each audience sees, pinned byte for byte: the operator
/// catalogue (and its HTTP and local-bridge views), worker sessions with and
/// without improve, room tools, the chat MCP bridge, the context
/// catalogue's index text and the shared agent guidance. Reshaping a
/// definition's source must not change it; an intended schema change
/// regenerates the fixture.
#[test]
fn tool_catalogue_matches_the_pinned_snapshot() {
    const FIXTURE: &str = include_str!("../../tests/fixtures/tool_catalogue.json");
    let all = control::permissions::Approvals {
        folder: true,
        port: true,
        bot: true,
        question: true,
        knowledge: true,
    };
    // Views that select from a full listing pin only their names.
    let names = |tools: Vec<Value>| tools.iter().map(|t| t["name"].clone()).collect::<Vec<_>>();
    let catalogue = json!({
        "operator": entries().iter().map(|e| &e.definition).collect::<Vec<_>>(),
        "operator_http": names(http_definitions()),
        "operator_local": names(local_definitions()),
        "worker": names(worker_definitions(false)),
        "worker_improve": worker_definitions(true),
        "worker_rooms": crate::rooms::worker_definitions(false),
        "worker_rooms_judge": crate::rooms::worker_definitions(true),
        "chat_bridge": crate::mcp_chat::definitions(false, all),
        "chat_bridge_read_only": names(crate::mcp_chat::definitions(true, Default::default())),
        "context_tools": control_context::TOOLS.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>(),
        "context_policies": control_context::tool_policy::POLICIES.iter().map(|p| format!("{p:?}")).collect::<Vec<_>>(),
        "context_guidance": [
            control_context::agent_policy::CODE_LANGUAGE_GUIDANCE,
            control_context::agent_policy::ROOM_COMMUNICATION_GUIDANCE,
            control_context::agent_policy::ACTIVE_SESSION_GUIDANCE,
            control_context::agent_policy::ROOM_GUIDANCE_DETAIL,
            control_context::agent_policy::SESSION_GUIDANCE_DETAIL,
        ],
    });
    let actual = serde_json::to_string_pretty(&catalogue).unwrap() + "\n";
    if actual != FIXTURE {
        let path = std::env::temp_dir().join("control-tool-catalogue.json");
        std::fs::write(&path, &actual).unwrap();
        let (line, (want, got)) = FIXTURE
            .lines()
            .chain(std::iter::repeat(""))
            .zip(actual.lines().chain(std::iter::repeat("")))
            .enumerate()
            .find(|(_, (want, got))| want != got)
            .unwrap();
        panic!(
            "Tool catalogue changed at line {}:\n  pinned: {want}\n  actual: {got}\nFull output: {}\n\
             If the change is intended: cp {} tests/fixtures/tool_catalogue.json",
            line + 1,
            path.display(),
            path.display()
        );
    }
}
