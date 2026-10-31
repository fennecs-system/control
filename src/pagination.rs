//! Small cursor windows over newest-first read projections. Saved IDs anchor
//! pages so newly appended history does not shift an older page's boundary.
pub const SIZE: usize = 10;
pub fn window(total: usize, older: Option<usize>, newer: Option<usize>) -> std::ops::Range<usize> {
    let range = if let Some(index) = older.filter(|index| *index < total) {
        let start = index + 1;
        start..start.saturating_add(SIZE).min(total)
    } else if let Some(index) = newer.filter(|index| *index < total) {
        index.saturating_sub(SIZE)..index
    } else {
        0..SIZE.min(total)
    };
    if range.is_empty() {
        0..SIZE.min(total)
    } else {
        range
    }
}
pub fn controls(base: &str, newer: Option<&str>, older: Option<&str>) -> String {
    let mut html = String::new();
    for (direction, label, id) in [("newer", "← Newer", newer), ("older", "Older →", older)] {
        if let Some(id) = id {
            let mut url = reqwest::Url::parse("http://localhost")
                .unwrap()
                .join(base)
                .unwrap();
            url.query_pairs_mut().append_pair(direction, id);
            let target = format!("{}?{}", url.path(), url.query().unwrap());
            html.push_str(&format!("<a href='{}'>{label}</a>", crate::escape(&target)));
        }
    }
    if html.is_empty() {
        html
    } else {
        format!("<div class='history-controls' aria-label='History pages'>{html}</div>")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_are_bounded_and_stale_cursors_return_to_latest() {
        assert_eq!(window(0, None, None), 0..0);
        assert_eq!(window(25, None, None), 0..10);
        assert_eq!(window(25, Some(9), None), 10..20);
        assert_eq!(window(25, Some(19), None), 20..25);
        assert_eq!(window(25, None, Some(20)), 10..20);
        assert_eq!(window(25, Some(usize::MAX), None), 0..10);
        assert_eq!(window(25, Some(24), None), 0..10);
        assert_eq!(window(25, None, Some(0)), 0..10);
        let html = controls("/?view=runs", Some("r &x"), None);
        assert!(html.contains("view=runs&amp;newer=r+%26x"));
    }
}
