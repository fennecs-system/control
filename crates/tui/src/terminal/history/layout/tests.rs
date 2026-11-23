use super::*;
#[test]
fn wrapped_ranges_preserve_utf8_combining_characters_and_span_styles() {
    for width in 1..24 {
        let original = "alpha beta猫 cafe\u{301} verylongword gamma";
        let mut cache = Cache::default();
        cache.update(Text::from(original.to_owned()), width);
        let result = cache
            .window(0, cache.len())
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<String>();
        assert_eq!(result, original, "width {width}");
        let mut line = Line::from(vec![
            Span::raw("command\t\n"),
            Span::styled("猫 café", Style::default().fg(ratatui::style::Color::Red)),
        ]);
        line.style = Style::default().bg(super::super::CARD);
        cache.update(Text::from(line), width);
        assert!(
            cache
                .window(0, cache.len())
                .lines
                .iter()
                .all(|line| line.style.bg == Some(super::super::CARD))
        );
    }
}
#[test]
fn scrolling_beyond_u16_rows_and_resizing_only_indexes_retained_text() {
    let text = Text::from(
        (0..100_000)
            .map(|n| Line::from(format!("row {n}")))
            .collect::<Vec<_>>(),
    );
    let mut cache = Cache::default();
    cache.update(text, 80);
    assert!(!cache.dirty());
    assert_eq!(cache.len(), 100_000);
    assert_eq!(cache.window(90_000, 1).to_string(), "row 90000");
    assert_eq!(cache.window(99_999, 30).lines.len(), 1);
    assert!(cache.window(100_001, 30).lines.is_empty());
    cache.resize(4);
    assert!(cache.len() > 100_000);
    assert!(!cache.dirty());
    cache.invalidate();
    assert!(cache.dirty());
}
