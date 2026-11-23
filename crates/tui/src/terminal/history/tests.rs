use super::*;
use ratatui::text::Text;
#[test]
#[ignore = "explicit offline visual preview; writes synthetic history to CONTROL_TUI_PREVIEW_HTML"]
fn preview() {
    use std::fmt::Write;
    let app = crate::terminal::App {
        detail: serde_json::json!({"messages":[
            {"kind":"chat","text":"Can you inspect the failing build and propose a small fix?\nKeep the change focused and preserve the existing API.","reply":"I found a mismatch between the cached output and the current schema.\nI’ll update the shared constructor, then run the affected checks.","status":"completed"},
            {"kind":"chat","text":"Please use a stronger model for the review, and show me the result.","reply":"The reviewer confirmed the behavior is preserved.\n\nChecks passed\n  cargo test --locked --workspace\n  cargo clippy --locked --workspace --all-targets -- -D warnings\n\nThe change is ready for review.","status":"completed"},
            {"kind":"chat","text":"Great. Check the remaining callers too.","reply":"Inspecting the caller paths and existing tests…","status":"working"}
        ]}),
        ..Default::default()
    };
    let area = Rect::new(0, 0, 96, 36);
    let mut buffer = Buffer::empty(area);
    History {
        paragraph: Paragraph::new(app.transcript()),
        scroll: 0,
    }
    .render(area, &mut buffer);
    let mut html = String::from(
        "<!doctype html><meta charset='utf-8'><title>Control terminal history preview</title><body style='margin:0;background:#0c0d10'><svg xmlns='http://www.w3.org/2000/svg' width='864' height='648' font-family='monospace' font-size='14'>",
    );
    let color = |color: Color, default: &str| match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => default.into(),
    };
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            let bg = color(cell.bg, "#0c0d10");
            let fg = color(cell.fg, "#e9e8ed");
            let text = cell
                .symbol()
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            write!(html,"<rect x='{}' y='{}' width='9' height='18' fill='{bg}'/><text x='{}' y='{}' fill='{fg}'>{text}</text>",x*9,y*18,x*9,y*18+14).unwrap();
        }
    }
    html.push_str("</svg></body>");
    std::fs::write(
        std::env::var("CONTROL_TUI_PREVIEW_HTML").expect("set the preview destination"),
        html,
    )
    .unwrap();
}
#[test]
fn wrapped_user_rows_have_a_continuous_card_and_keep_all_characters() {
    let text = Text::from(vec![
        user_line(Line::from("alpha beta gamma delta")),
        Line::from("assistant reply"),
    ]);
    let area = Rect::new(0, 0, 16, 8);
    let mut buffer = Buffer::empty(area);
    History {
        paragraph: Paragraph::new(text),
        scroll: 0,
    }
    .render(area, &mut buffer);
    let rows = (0..8)
        .map(|y| (0..16).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>();
    assert!(rows[0].contains("alpha beta"));
    assert!(rows[1].contains("gamma delta"));
    for y in [0, 1] {
        assert_eq!(buffer[(15, y)].bg, CARD);
        assert_eq!(buffer[(0, y)].symbol(), "▏");
    }
    assert_ne!(buffer[(15, 2)].bg, CARD);
}
#[test]
fn scroll_and_narrow_viewports_do_not_overwrite_unicode_or_leave_old_card_background() {
    for width in 0..12 {
        let area = Rect::new(0, 0, width, 4);
        let mut buffer = Buffer::empty(area);
        for scroll in [0, 1, 8] {
            History {
                paragraph: Paragraph::new(Text::from(vec![
                    user_line(Line::from("猫 café")),
                    Line::from("reply"),
                ])),
                scroll,
            }
            .render(area, &mut buffer);
        }
    }
    let area = Rect::new(0, 0, 20, 3);
    let mut buffer = Buffer::empty(area);
    History {
        paragraph: Paragraph::new(Text::from(user_line(Line::from("question")))),
        scroll: 0,
    }
    .render(area, &mut buffer);
    History {
        paragraph: Paragraph::new("reply"),
        scroll: 0,
    }
    .render(area, &mut buffer);
    assert_ne!(buffer[(2, 0)].bg, CARD);
}
