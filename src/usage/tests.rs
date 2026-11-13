
use super::*;
#[test]
fn heatmap_spans_whole_weeks_and_shades_relative_to_the_busiest_day() {
    // 2026-11-03 is a Tuesday: 25 full weeks plus Sunday..Tuesday.
    let today = 20_760;
    assert_eq!(day_label(today * 86400), "2026-11-03");
    let daily = BTreeMap::from([(today, 400), (today - 1, 100), (today - 400, 9_999)]);
    let html = heatmap(&daily, today);
    assert_eq!(
        html.matches("<span class='l").count() - 5,
        25 * 7 + 3,
        "cells excluding the legend"
    );
    assert!(
        html.contains("title='2026-11-03: 400 tokens'")
            && html.contains("class='l4' title='2026-11-03")
    );
    assert!(
        html.contains("class='l1' title='2026-11-02: 100 tokens'"),
        "older out-of-range days do not set the scale"
    );
    assert!(
        html.contains("<span class='l0' title='2026-05-10"),
        "the first column starts on a Sunday"
    );
}
#[test]
fn outcomes_keep_statuses_exact_and_bound_visible_models() {
    let mut models = BTreeMap::<String, Summary>::new();
    for status in [
        "completed",
        "completed",
        "failed",
        "interrupted",
        "running",
        "blocked",
        "unknown",
    ] {
        models
            .entry("Claude · '<model>'".into())
            .or_default()
            .add(&Attempt {
                status: status.into(),
                ..Attempt::default()
            });
    }
    let html = outcomes(&models);
    assert!(html.contains("2 completed, 1 failed, 1 interrupted, 3 other attempts"));
    assert_eq!(html.matches("<rect ").count(), 4);
    assert!(html.contains("Claude · &#39;&lt;model&gt;&#39;"));
    assert!(!html.contains("<model>"));
    assert!(html.contains("not verified task success"));
    for index in 0..10 {
        models
            .entry(format!("model-{index}"))
            .or_default()
            .add(&Attempt::default());
    }
    let html = outcomes(&models);
    assert_eq!(html.matches("<svg ").count(), 6);
    assert!(html.contains("Six most-used models"));
    assert!(outcomes(&BTreeMap::new()).contains("No recorded attempts yet"));
}

#[test]
#[ignore = "local bounded outcome rendering measurement"]
fn profile_outcome_rendering() {
    let models: BTreeMap<_, _> = (0..64)
        .map(|model| {
            (
                format!("provider · model-{model}"),
                Summary {
                    attempts: 100,
                    completed: 80,
                    failed: 10,
                    interrupted: 5,
                    ..Summary::default()
                },
            )
        })
        .collect();
    let start = std::time::Instant::now();
    for _ in 0..10_000 {
        std::hint::black_box(outcomes(std::hint::black_box(&models)));
    }
    eprintln!(
        "Six outcome bars / 64 models: {:?} per render",
        start.elapsed() / 10_000
    );
}

#[test]
fn structured_activity_keeps_tools_bounded_and_excludes_reasoning() {
    let events = format!(
        "{}\n{}\n{}\n{}",
        serde_json::json!({"type":"item.started","item":{"id":"cmd1","type":"command_execution","command":"cargo check","status":"in_progress"}}),
        serde_json::json!({"type":"item.completed","item":{"id":"cmd1","type":"command_execution","command":"cargo check","status":"completed","aggregated_output":format!("done\u{1b}[31m{}", "x".repeat(5000))}}),
        serde_json::json!({"type":"item.completed","item":{"id":"thought","type":"reasoning","text":"private reasoning"}}),
        serde_json::json!({"type":"turn.completed","usage":{"input_tokens":12,"cached_input_tokens":4,"output_tokens":8}})
    );
    let mut activity = Vec::new();
    let tokens = capture_with(events.as_bytes(), |event| activity.push(event)).unwrap();
    assert_eq!(tokens.output_tokens, 8);
    assert_eq!(activity.len(), 2);
    assert_eq!(activity[0].text, "cargo check");
    assert_eq!(activity[1].output.chars().count(), 4000);
    assert!(!activity[1].output.contains('\u{1b}'));
    let message=control_ai::codex::activity(&serde_json::json!({"type":"item.completed","item":{"id":"visible-message","type":"agent_message","text":"Checking the bot connection."}})).unwrap();
    assert_eq!(message.kind, "message");
    assert_eq!(message.text, "Checking the bot connection.");
    let failed = control_ai::codex::activity(&serde_json::json!({"type":"item.completed","item":{"id":"mcp1","type":"mcp_tool_call","server":"control","tool":"control_port_request","arguments":{},"error":{"message":"Egress requires exactly one of target or peer_port_id"},"status":"failed"}})).unwrap();
    assert_eq!(
        (failed.text.as_str(), failed.status.as_str()),
        ("control_port_request", "failed")
    );
    assert!(failed.output.contains("peer_port_id"));
    let ok = control_ai::codex::activity(&serde_json::json!({"type":"item.completed","item":{"id":"mcp2","type":"mcp_tool_call","server":"control","tool":"control_port_status","arguments":{},"result":{"content":[{"type":"text","text":"ready"},{"type":"image","data":"x"},{"type":"text","text":"127.0.0.1:25565\u{1b}"}],"structured_content":{"ready":true}},"error":null,"status":"completed"}})).unwrap();
    assert_eq!(
        (ok.status.as_str(), ok.output.as_str()),
        ("completed", "ready\n127.0.0.1:25565")
    );
    let rejected = control_ai::codex::activity(&serde_json::json!({"type":"item.completed","item":{"id":"mcp3","type":"mcp_tool_call","server":"control","tool":"control_port_request","arguments":{},"result":{"content":[{"type":"text","text":"Peer ingress is not owned by this chat"}],"structured_content":null},"error":null,"status":"failed"}})).unwrap();
    assert_eq!(
        (rejected.status.as_str(), rejected.output.as_str()),
        ("failed", "Peer ingress is not owned by this chat")
    );
    let large = "y".repeat(10_000);
    let bounded = control_ai::codex::activity(&serde_json::json!({"type":"item.completed","item":{"id":"mcp4","type":"mcp_tool_call","server":"control","tool":"control_service_exec_status","arguments":{},"result":{"content":[{"type":"text","text":large}]},"status":"completed"}})).unwrap();
    assert_eq!(bounded.output.chars().count(), 1000);
    let started = control_ai::codex::activity(&serde_json::json!({"type":"item.started","item":{"id":"mcp5","type":"mcp_tool_call","server":"control","tool":"control_port_status","arguments":{},"result":null,"error":null,"status":"in_progress"}})).unwrap();
    assert_eq!(started.output, "");
}
#[test]
fn aggregates_retries_preserves_legacy_runs_and_escapes_content() {
    let mut state = crate::State::default();
    let mut run: crate::runtime::Run = serde_json::from_value(serde_json::json!({
            "id":"sample", "objective":"Review <script>bad</script>", "kind":"task", "status":"completed", "source":"website", "result":""
        })).unwrap();
    assert!(run.attempts.is_empty());
    run.actor = Actor::LocalOperator;
    run.attempts = vec![
        Attempt {
            model: "gpt-6.1-sol".into(),
            provider: "OpenAI / Codex".into(),
            tokens: Some(Tokens {
                input_tokens: 100,
                cached_input_tokens: 80,
                output_tokens: 20,
            }),
            status: "failed".into(),
            ..Attempt::default()
        },
        Attempt {
            model: "gpt-6.1-sol".into(),
            provider: "OpenAI / Codex".into(),
            tokens: Some(Tokens {
                input_tokens: 50,
                cached_input_tokens: 10,
                output_tokens: 5,
            }),
            status: "completed".into(),
            ..Attempt::default()
        },
    ];
    state.runtime.runs.push(run);
    let html = render(&state.runtime);
    assert!(html.contains("<b>150</b><span>INPUT"));
    assert!(html.contains("<b>25</b><span>OUTPUT"));
    assert!(html.contains("2/2"));
    assert!(
        !html.contains("script&gt;bad") && !html.contains("<script>bad"),
        "run objectives stay in run history"
    );
    assert!(html.contains("href='/?view=runs'"));
    if let Ok(directory) = std::env::var("CONTROL_PREVIEW_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        for (view, file) in [
            ("monitor", "usage.html"),
            ("hub", "overview.html"),
            ("agents", "agents.html"),
            ("lenses", "lenses.html"),
            ("rooms", "rooms.html"),
            ("session-list", "chat.html"),
        ] {
            let channel = if view == "session-list" {
                crate::chat::landing(&state, "research", Some("knowledge"))
            } else {
                "general".into()
            };
            let page = crate::render(&state, view, "research", &channel)
                .replace(
                    "<link rel=\"stylesheet\" href=\"/style.css\">",
                    &format!("<style>{}</style>", include_str!("../../assets/style.css")),
                )
                .replace(
                    "<link rel='stylesheet' href='/collective.css'>",
                    &format!(
                        "<style>{}</style>",
                        include_str!("../../assets/collective.css")
                    ),
                )
                .replace(
                    "<link rel='stylesheet' href='/runtime.css'>",
                    &format!(
                        "<style>{}</style>",
                        include_str!("../../assets/runtime.css")
                    ),
                );
            std::fs::write(std::path::Path::new(&directory).join(file), page).unwrap();
        }
    }
}
#[test]
fn accounting_ignores_chat_malformed_events_and_counts_cache_once() {
    let events = b"{\"type\":\"item.completed\",\"usage\":{\"input_tokens\":999}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"cached_input_tokens\":80,\"output_tokens\":20}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":50,\"cached_input_tokens\":10,\"output_tokens\":5}}\n{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":-5}}";
    let tokens = capture(&events[..]).unwrap();
    assert_eq!(
        tokens,
        Tokens {
            input_tokens: 150,
            cached_input_tokens: 90,
            output_tokens: 25
        }
    );
    assert!(capture(&b"broken\n{\"type\":\"turn.failed\"}"[..]).is_none());
}
#[test]
fn days_and_legacy_attempts_keep_unknown_distinct_from_zero() {
    assert_eq!(day_label(0), "1970-01-01");
    assert_eq!(day_label(1793145600), "2026-10-28");
    let mut sum = Summary::default();
    sum.add(&Attempt::default());
    sum.add(&Attempt {
        tokens: Some(Tokens::default()),
        ..Attempt::default()
    });
    assert_eq!((sum.attempts, sum.measured), (2, 1));
}
