
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
