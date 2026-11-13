//! Transcript presentation only. Layout work is bounded by the visible viewport.
pub(super) mod layout;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Paragraph, Widget, Wrap},
};
pub(super) const CARD: Color = Color::Rgb(31, 31, 38);
pub(super) const PURPLE: Color = Color::Rgb(181, 160, 255);
pub(super) fn user_line(mut line: Line<'static>) -> Line<'static> {
    line.style = Style::default().fg(Color::Rgb(233, 232, 237)).bg(CARD);
    line
}
fn gutter(width: u16) -> u16 {
    if width >= 8 { 2 } else { 0 }
}
pub(super) fn content_width(width: u16) -> u16 {
    width.saturating_sub(gutter(width) * 2)
}
pub(super) struct History<'a> {
    pub paragraph: Paragraph<'a>,
    pub scroll: u16,
}
impl Widget for History<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let inset = gutter(area.width);
        let body = Rect::new(
            area.x + inset,
            area.y,
            content_width(area.width),
            area.height,
        );
        self.paragraph
            .style(
                Style::default()
                    .bg(Color::Rgb(12, 13, 16))
                    .fg(Color::Rgb(233, 232, 237)),
            )
            .wrap(Wrap { trim: false })
            .scroll((self.scroll, 0))
            .render(body, buffer);
        // Paragraph styles only populated cells. Extend a user card across its
        // wrapped rows without pre-wrapping or padding every historical message.
        for y in body.top()..body.bottom() {
            if (body.left()..body.right()).any(|x| buffer[(x, y)].bg == CARD) {
                buffer.set_style(
                    Rect::new(area.x, y, area.width, 1),
                    Style::default().bg(CARD),
                );
                if inset > 0 {
                    buffer[(area.x, y)].set_symbol("▏").set_fg(PURPLE);
                }
            }
        }
    }
}
#[cfg(test)]
mod tests;
