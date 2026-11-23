use super::*;
use std::{hint::black_box, time::Instant};
#[test]
#[ignore = "explicit cached large-history scrolling benchmark, no model traffic"]
fn scroll_profile() {
    let rounds = std::env::var("CONTROL_TUI_PROFILE_ROUNDS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 10000);
    for count in [1000, 10000] {
        let mut app = App {
            detail: json!({"messages":(0..count).map(|n|json!({"text":format!("Task {n}: verify the next change."),"reply":"Verified. Continue with the next useful action.","status":"completed","kind":"chat"})).collect::<Vec<_>>()}),
            ..Default::default()
        };
        let cold = Instant::now();
        app.layout_history(100);
        let cold_ms = cold.elapsed().as_millis();
        let rows = app.history.len();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        let mut samples = Vec::new();
        for batch in 0..10 {
            let start = Instant::now();
            for iteration in 0..rounds {
                app.layout_history(100);
                let offset = (iteration * 97 + batch * 113) % rows.saturating_sub(30).max(1);
                let text = black_box(app.history.window(offset, 30));
                terminal
                    .draw(|frame| {
                        frame.render_widget(
                            history::History {
                                paragraph: Paragraph::new(text),
                                scroll: 0,
                            },
                            frame.area(),
                        )
                    })
                    .unwrap();
            }
            if batch > 0 {
                samples.push(start.elapsed().as_nanos() / rounds as u128);
            }
        }
        samples.sort_unstable();
        println!(
            "{}",
            json!({"workload":"terminal_scroll","messages":count,"rows":rows,"cold_ms":cold_ms,"median_ns":samples[4],"samples":samples})
        );
    }
}
#[test]
#[ignore = "explicit offline terminal-render timing/flamegraph; no server, credentials or model calls"]
fn render_profile() {
    let rounds = std::env::var("CONTROL_TUI_PROFILE_ROUNDS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(1, 10000);
    for count in [10, 100, 500] {
        let messages=(0..count).map(|index|json!({"text":format!("Task {index}: inspect the next useful change."),"status":"completed","kind":"chat","reply":"Verified the change.\nNext: continue with the task.\n","activities":[]})).collect::<Vec<_>>();
        let mut app = App {
            active: "fixture".into(),
            detail: json!({"messages":messages}),
            ..Default::default()
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        let mut samples = Vec::new();
        for batch in 0..10 {
            let start = Instant::now();
            for _ in 0..rounds {
                app.layout_history(100);
                let transcript = black_box(app.history.window(0, 30));
                terminal
                    .draw(|frame| {
                        frame.render_widget(
                            history::History {
                                paragraph: Paragraph::new(transcript),
                                scroll: 0,
                            },
                            frame.area(),
                        )
                    })
                    .unwrap();
            }
            if batch > 0 {
                samples.push(start.elapsed().as_nanos() / rounds as u128);
            }
        }
        samples.sort_unstable();
        let output = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let hash = ring::digest::digest(&ring::digest::SHA256, output.as_bytes())
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        println!(
            "{}",
            json!({"workload":"terminal_render","messages":count,"median_ns":samples[4],"samples":samples,"sha256":hash,"columns":100,"rows":30})
        );
    }
}

#[test]
#[ignore = "offline command discovery timing; no server, credentials or model calls"]
fn command_profile() {
    for input in ["/", "/con", "/context", "/improve", "ordinary text"] {
        let app = App {
            input: input.into(),
            ..Default::default()
        };
        let mut samples = Vec::new();
        for batch in 0..10 {
            let start = Instant::now();
            for _ in 0..100_000 {
                black_box(app.command_matches());
                black_box(command_line(black_box(input)));
            }
            if batch > 0 {
                samples.push(start.elapsed().as_nanos() / 100_000);
            }
        }
        samples.sort_unstable();
        println!(
            "{}",
            json!({"workload":"terminal_commands","input":input,"median_ns":samples[4]})
        );
    }
}

/// Per-keystroke composer cost: draft bookkeeping plus the wrapped input paragraph,
/// against the largest lens library and longest draft the server accepts.
#[test]
#[ignore = "offline composer keystroke timing; no server, credentials or model calls"]
fn composer_profile() {
    let library = (0..4096)
        .map(|n| json!({"id": format!("lens-{n:04}")}))
        .collect::<Vec<_>>();
    let names = |count: usize| {
        (0..count)
            .map(|n| format!("lens-{:04}", n * 37 % 8192))
            .collect::<Vec<_>>()
    };
    let mut long = format!("/use {}", names(4000).join(", "));
    long.truncate(control_context::limits::CHAT_MESSAGE_CHARS);
    let prose = "verify the next change ".repeat(870);
    for (workload, input) in [
        ("use_short", "/use lens-0001 lens-9999".to_owned()),
        ("use_selected_max", format!("/use {}", names(128).join(", "))),
        ("use_long", long),
        ("prose_long", prose),
    ] {
        let mut app = App {
            input: input.clone(),
            lenses: lens_input::Catalog::from_list(&json!(library)),
            ..Default::default()
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 5)).unwrap();
        let rounds = 200;
        let mut samples = Vec::new();
        for batch in 0..10 {
            let start = Instant::now();
            for _ in 0..rounds {
                app.input_changed();
                let input = clean(&app.input);
                let lines: Vec<_> = input
                    .split('\n')
                    .map(|line| {
                        let mut spans = vec![Span::raw("> ")];
                        spans.extend(composer_line(line, &app.lenses).spans);
                        Line::from(spans)
                    })
                    .collect();
                let text = black_box(Text::from(lines));
                terminal
                    .draw(|frame| frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), frame.area()))
                    .unwrap();
            }
            if batch > 0 {
                samples.push(start.elapsed().as_nanos() / rounds as u128);
            }
        }
        samples.sort_unstable();
        println!(
            "{}",
            json!({"workload": "terminal_composer", "input": workload, "chars": input.chars().count(), "lenses": library.len(), "median_ns": samples[4]})
        );
    }
}
