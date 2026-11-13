
use super::*;
use std::{hint::black_box, time::Instant};
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
        let app = App {
            active: "fixture".into(),
            detail: json!({"messages":messages}),
            ..Default::default()
        };
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        let mut samples = Vec::new();
        for batch in 0..10 {
            let start = Instant::now();
            for _ in 0..rounds {
                let transcript = black_box(app.transcript());
                terminal
                    .draw(|frame| {
                        frame.render_widget(
                            Paragraph::new(transcript).wrap(Wrap { trim: false }),
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
