
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
                .any(|t| t.mcp_names.contains(&entry.name())),
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
    assert!(!worker_definitions(false).iter().any(|tool|tool["name"]=="control_github_read"));
    let mut seen = std::collections::HashSet::new();
    for &tools in &ATTEMPT_TOOLS {
        let definitions = tools.definitions();
        assert!(!definitions.is_empty());
        for definition in definitions {
            let name = definition["name"].as_str().unwrap();
            assert_eq!(
                ATTEMPT_TOOLS
                    .iter()
                    .filter(|other| other.known(name))
                    .count(),
                1,
                "{name}"
            );
            assert!(
                std::ptr::addr_eq(attempt_tools(name).unwrap(), tools),
                "{name}"
            );
            assert!(
                workers.iter().any(|worker| worker["name"] == name),
                "{name}"
            );
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
