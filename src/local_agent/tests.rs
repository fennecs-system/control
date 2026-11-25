//! The local loop's host-free decisions against the worker tools this node
//! actually serves.
use control_local::{functions, result_text, searched, size, tokens};
use serde_json::json;

#[test]
fn worker_tool_schemas_fit_the_declared_overhead_reserve() {
    use crate::worker_tools::deferred::{Facts, listed, offered};
    use control_context::Audience;
    for role in Audience::ALL {
        for improve in [false, true] {
            let facts = Facts {
                role,
                improve,
                managed: true,
                judge: role == Audience::Advisor,
                advisor: role == Audience::Advisor,
                shell: true,
                local: true,
                rooms: true,
                standing: crate::personas::Effective::builtin("worker", false),
            };
            let definitions = listed(&facts);
            let tools = functions(&definitions);
            let estimate = tokens(size(&tools));
            let all = tokens(size(&functions(&offered(&facts))));
            println!("{role:?} improve={improve}: core {estimate} tokens, every callable tool {all} tokens");
            // The reserve covers the role core, schemas loaded during the turn and one exchange.
            assert!(
                estimate <= control_local::CORE_TOKENS,
                "core schemas need {estimate} tokens; raise CORE_TOKENS"
            );
            // Names and parameter schemas are passed through unchanged.
            for (tool, definition) in tools.iter().zip(&definitions) {
                assert_eq!(tool["function"]["name"], definition["name"]);
                assert_eq!(tool["function"]["parameters"], definition["inputSchema"]);
            }
        }
    }
}
#[test]
fn a_search_result_names_loaded_tools_instead_of_repeating_their_schemas() {
    let schemas = crate::tool_registry::worker_definitions(false)
        .into_iter()
        .filter(|t| {
            matches!(
                t["name"].as_str(),
                Some("control_service_exec" | "control_watch_stop" | "control_port_status")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(schemas.len(), 3);
    let note = "Host note: one more thing.";
    let mut full = crate::toolbox::result(Ok(
        json!({"tools":schemas,"next":"These tools are now callable directly by name."}),
    ));
    full["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"text","text":note}));
    let before = result_text(&full).0.len();
    let shown = searched(full);
    let (text, failed) = result_text(&shown);
    assert!(
        !failed && !text.contains("inputSchema") && text.ends_with(note),
        "{text}"
    );
    assert!(text.contains(r#""name":"control_service_exec""#) && text.contains("callable directly"));
    for line in shown["structuredContent"]["data"]["tools"].as_array().unwrap() {
        assert!(line["description"].as_str().unwrap().len() <= 160, "{line}");
    }
    eprintln!("local search result: before {before} after {} bytes", text.len());
    assert!(text.len() * 3 < before, "before {before} after {}", text.len());
    // Errors and empty results pass through unchanged.
    let refused = json!({"content":[{"type":"text","text":"refused"}],"isError":true});
    assert_eq!(searched(refused.clone()), refused);
}
