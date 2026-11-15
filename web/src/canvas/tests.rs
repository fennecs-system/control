use super::*;

fn close(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
}

#[test]
fn view_boxes_parse_strictly() {
    assert_eq!(Rect::parse("-2 -2 640 220"), Some(Rect { x: -2.0, y: -2.0, w: 640.0, h: 220.0 }));
    assert_eq!(Rect::parse("0,0, 10 5"), Some(Rect { x: 0.0, y: 0.0, w: 10.0, h: 5.0 }));
    assert_eq!(Rect::parse("0 0 10"), None);
    assert_eq!(Rect::parse("0 0 10 5 1"), None);
    assert_eq!(Rect::parse("0 0 0 5"), None);
    assert_eq!(Rect::parse("0 0 NaN 5"), None);
}

#[test]
fn a_new_view_fits_and_centres_the_drawing() {
    let bounds = Rect { x: -2.0, y: -2.0, w: 2000.0, h: 400.0 };
    let view = View::new(bounds, 1000.0, 600.0);
    assert_eq!(view.scale, 0.5, "the wider dimension decides");
    assert!(close(view.point(0.0, 300.0), (-2.0, 198.0)), "left edge, vertically centred");
    assert!(close(view.point(1000.0, 300.0), (1998.0, 198.0)));
    assert_eq!(view.view_box(), "-2.00 -402.00 2000.00 1200.00", "the viewBox keeps the element's aspect");
    let small = View::new(Rect { x: 0.0, y: 0.0, w: 100.0, h: 50.0 }, 1000.0, 600.0);
    assert_eq!(small.scale, 1.0, "small drawings are not enlarged");
    assert!(close(small.point(500.0, 300.0), (50.0, 25.0)));
}

#[test]
fn zoom_keeps_the_point_under_the_cursor_and_is_bounded() {
    let mut view = View::new(Rect { x: 0.0, y: 0.0, w: 2000.0, h: 400.0 }, 1000.0, 600.0);
    let under = view.point(250.0, 120.0);
    view.zoom(3.0, 250.0, 120.0);
    assert_eq!(view.scale, 1.5);
    assert!(close(view.point(250.0, 120.0), under));
    view.zoom(100.0, 0.0, 0.0);
    assert_eq!(view.scale, MAX_SCALE);
    view.zoom(1e-6, 0.0, 0.0);
    assert_eq!(view.scale, 0.25, "zooming out stops at half the fitted size");
    view.zoom(f64::NAN, 0.0, 0.0);
    assert_eq!(view.scale, 0.25);
}

#[test]
fn panning_moves_the_drawing_with_the_pointer_and_fit_resets() {
    let mut view = View::new(Rect { x: 0.0, y: 0.0, w: 2000.0, h: 400.0 }, 1000.0, 600.0);
    let fitted = view;
    let before = view.point(400.0, 300.0);
    view.pan(100.0, -50.0);
    assert!(close(view.point(500.0, 250.0), before), "what was under the pointer follows it");
    view.resize(800.0, 600.0);
    assert!(close(view.point(500.0, 250.0), before), "a moved view keeps its place when resized");
    view.resize(1000.0, 600.0);
    view.fit();
    assert_eq!(view, fitted);
    let mut untouched = fitted;
    untouched.resize(500.0, 600.0);
    assert_eq!(untouched.scale, 0.25, "an untouched view refits when resized");
}

#[test]
fn wheel_zoom_is_symmetric_and_pinches_are_faster() {
    let (inward, outward) = (wheel_factor(-100.0, 0, false, 600.0), wheel_factor(100.0, 0, false, 600.0));
    assert!(inward > 1.0 && (inward * outward - 1.0).abs() < 1e-12);
    assert!(wheel_factor(-10.0, 0, true, 600.0) > wheel_factor(-10.0, 0, false, 600.0));
    assert_eq!(wheel_factor(-3.0, 1, false, 600.0), wheel_factor(-48.0, 0, false, 600.0), "line deltas are 16 px");
    assert_eq!(wheel_factor(1.0, 2, false, 600.0), wheel_factor(600.0, 0, false, 600.0), "page deltas are one canvas height");
}
