//! One retained transcript plus compact UTF-8 row ranges, rather than a cell grid.
use ratatui::{
    style::Style,
    text::{Line, Span, Text},
};
#[derive(Clone, Copy, Default)]
struct Point {
    span: usize,
    byte: usize,
}
struct Row {
    line: usize,
    start: Point,
    end: Point,
}
pub(crate) struct Cache {
    source: Text<'static>,
    rows: Vec<Row>,
    width: u16,
    dirty: bool,
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            source: Text::default(),
            rows: Vec::new(),
            width: 0,
            dirty: true,
        }
    }
}
impl Cache {
    pub(crate) fn invalidate(&mut self) {
        self.dirty = true;
    }
    pub(crate) fn dirty(&self) -> bool {
        self.dirty
    }
    pub(crate) fn update(&mut self, source: Text<'static>, width: u16) {
        self.source = source;
        self.dirty = false;
        self.resize(width);
    }
    pub(crate) fn width(&self) -> u16 {
        self.width
    }
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }
    pub(crate) fn resize(&mut self, width: u16) {
        self.width = width;
        self.rows.clear();
        for (line_index, line) in self.source.lines.iter().enumerate() {
            let mut start = Point::default();
            let mut end = start;
            let mut columns = 0usize;
            let mut last_break: Option<(Point, usize)> = None;
            for (span_index, span) in line.spans.iter().enumerate() {
                let mut byte = 0;
                for grapheme in span.styled_graphemes(Style::default()) {
                    // Ratatui omits control graphemes; retain offsets in the
                    // original UTF-8 string even when tabs/newlines were skipped.
                    byte += span.content[byte..]
                        .find(grapheme.symbol)
                        .expect("grapheme belongs to its source span");
                    let point = Point {
                        span: span_index,
                        byte,
                    };
                    byte += grapheme.symbol.len();
                    let next = Point {
                        span: span_index,
                        byte,
                    };
                    let cells = Span::raw(grapheme.symbol).width();
                    while width > 0 && columns > 0 && columns + cells > usize::from(width) {
                        let (boundary, used) = last_break.take().unwrap_or((point, columns));
                        self.rows.push(Row {
                            line: line_index,
                            start,
                            end: boundary,
                        });
                        start = boundary;
                        columns -= used;
                    }
                    columns += cells;
                    end = next;
                    if grapheme.symbol != "\u{a0}"
                        && grapheme.symbol.chars().all(char::is_whitespace)
                    {
                        last_break = Some((next, columns));
                    }
                }
            }
            self.rows.push(Row {
                line: line_index,
                start,
                end,
            });
        }
    }
    pub(crate) fn window(&self, start: usize, height: usize) -> Text<'_> {
        let rows = self
            .rows
            .get(start..start.saturating_add(height).min(self.rows.len()))
            .unwrap_or(&[]);
        Text::from(
            rows.iter()
                .map(|row| {
                    let source = &self.source.lines[row.line];
                    let mut spans = Vec::new();
                    for (index, span) in source
                        .spans
                        .iter()
                        .enumerate()
                        .skip(row.start.span)
                        .take(row.end.span.saturating_sub(row.start.span) + 1)
                    {
                        let start = if index == row.start.span {
                            row.start.byte
                        } else {
                            0
                        };
                        let end = if index == row.end.span {
                            row.end.byte
                        } else {
                            span.content.len()
                        };
                        if start < end {
                            spans.push(Span::styled(&span.content[start..end], span.style));
                        }
                    }
                    if spans.is_empty() {
                        spans.push(Span::raw(" "));
                    }
                    let mut line = Line::from(spans);
                    line.style = source.style;
                    line.alignment = source.alignment;
                    line
                })
                .collect::<Vec<_>>(),
        )
    }
}
#[cfg(test)]
mod tests;
