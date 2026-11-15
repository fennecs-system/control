//! Pan and zoom for server-rendered SVG drawings marked `data-canvas`. The
//! server's `viewBox` is the drawing's bounds; the client fits them to the
//! element, then rewrites the `viewBox` as the view moves. Links stay ordinary
//! links: a press only becomes a drag after it travels a few pixels.
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};

/// Pixels a press may travel and still count as a click.
const DRAG_SLOP: f64 = 4.0;
/// Largest zoom, in screen pixels per drawing unit.
const MAX_SCALE: f64 = 4.0;
/// Step for the zoom buttons and keys.
const STEP: f64 = 1.25;
/// Pixels moved per arrow key press.
const NUDGE: f64 = 48.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect { pub x: f64, pub y: f64, pub w: f64, pub h: f64 }
impl Rect {
    /// An SVG `viewBox` value: four numbers separated by spaces or commas.
    pub fn parse(view_box: &str) -> Option<Rect> {
        let mut numbers = view_box.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()).map(str::parse::<f64>);
        let rect = Rect { x: numbers.next()?.ok()?, y: numbers.next()?.ok()?, w: numbers.next()?.ok()?, h: numbers.next()?.ok()? };
        (numbers.next().is_none() && rect.w > 0.0 && rect.h > 0.0 && [rect.x, rect.y].iter().all(|v| v.is_finite())).then_some(rect)
    }
}

/// What part of the drawing is on screen: the drawing point at the element's
/// top-left corner and the screen pixels per drawing unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View { bounds: Rect, width: f64, height: f64, x: f64, y: f64, scale: f64, moved: bool }
impl View {
    pub fn new(bounds: Rect, width: f64, height: f64) -> View {
        let mut view = View { bounds, width: 1.0, height: 1.0, x: bounds.x, y: bounds.y, scale: 1.0, moved: false };
        view.resize(width, height);
        view
    }
    /// The scale at which the whole drawing fits, never enlarged past 1:1.
    fn fit_scale(&self) -> f64 {
        (self.width / self.bounds.w).min(self.height / self.bounds.h).min(1.0)
    }
    /// Centres the whole drawing in the element.
    pub fn fit(&mut self) {
        self.scale = self.fit_scale();
        self.x = self.bounds.x + (self.bounds.w - self.width / self.scale) / 2.0;
        self.y = self.bounds.y + (self.bounds.h - self.height / self.scale) / 2.0;
        self.moved = false;
    }
    /// A new element size refits an untouched view and otherwise keeps the
    /// drawing where the reader left it.
    pub fn resize(&mut self, width: f64, height: f64) {
        (self.width, self.height) = (width.max(1.0), height.max(1.0));
        if !self.moved {
            self.fit();
        }
    }
    /// Zooms by `factor` keeping the drawing point under (`px`, `py`) still.
    /// It can zoom out to half the fitted size and in to `MAX_SCALE`.
    pub fn zoom(&mut self, factor: f64, px: f64, py: f64) {
        let scale = (self.scale * factor).clamp(self.fit_scale() / 2.0, MAX_SCALE);
        if !scale.is_finite() || scale == self.scale {
            return;
        }
        let (x, y) = self.point(px, py);
        (self.x, self.y, self.scale) = (x - px / scale, y - py / scale, scale);
        self.moved = true;
    }
    /// Zooms about the middle of the element.
    pub fn zoom_centre(&mut self, factor: f64) {
        self.zoom(factor, self.width / 2.0, self.height / 2.0);
    }
    /// Moves the drawing by a screen distance.
    pub fn pan(&mut self, dx: f64, dy: f64) {
        self.x -= dx / self.scale;
        self.y -= dy / self.scale;
        self.moved = true;
    }
    /// The drawing point under a screen point within the element.
    pub fn point(&self, px: f64, py: f64) -> (f64, f64) {
        (self.x + px / self.scale, self.y + py / self.scale)
    }
    /// The `viewBox` showing this view; it has the element's aspect ratio.
    pub fn view_box(&self) -> String {
        format!("{:.2} {:.2} {:.2} {:.2}", self.x, self.y, self.width / self.scale, self.height / self.scale)
    }
}

/// The zoom factor for one wheel event. Trackpad pinches arrive as wheel
/// events with Ctrl held and small deltas, so they are scaled up.
pub fn wheel_factor(delta: f64, mode: u32, pinch: bool, page: f64) -> f64 {
    let pixels = match mode { 1 => delta * 16.0, 2 => delta * page, _ => delta };
    (-pixels * if pinch { 0.01 } else { 0.002 }).exp()
}

struct Canvas {
    root: web_sys::HtmlElement,
    svg: web_sys::Element,
    view: View,
    /// Pointers pressed on the canvas: id and last client position.
    pointers: Vec<(i32, f64, f64)>,
    travel: f64,
    dragged: bool,
}
impl Canvas {
    fn draw(&self) {
        let _ = self.svg.set_attribute("viewBox", &self.view.view_box());
    }
    fn origin(&self) -> (f64, f64) {
        let rect = self.root.get_bounding_client_rect();
        (rect.left(), rect.top())
    }
    fn press(&mut self, event: &web_sys::PointerEvent) {
        let controls = event.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()).and_then(|t| t.closest(".canvas-controls").ok().flatten());
        if controls.is_some() || (event.pointer_type() == "mouse" && event.button() != 0) {
            return;
        }
        if self.pointers.is_empty() {
            (self.travel, self.dragged) = (0.0, false);
        }
        self.pointers.retain(|(id, ..)| *id != event.pointer_id());
        self.pointers.push((event.pointer_id(), event.client_x() as f64, event.client_y() as f64));
    }
    fn grab(&mut self) {
        if !self.dragged {
            self.dragged = true;
            let _ = self.root.class_list().add_1("dragging");
        }
        for (id, ..) in &self.pointers {
            let _ = self.root.set_pointer_capture(*id);
        }
    }
    fn drag(&mut self, event: &web_sys::PointerEvent) {
        let Some(index) = self.pointers.iter().position(|(id, ..)| *id == event.pointer_id()) else { return };
        if event.buttons() == 0 {
            // Released outside the page before any drag captured it.
            return self.release(event);
        }
        let (x, y) = (event.client_x() as f64, event.client_y() as f64);
        let (_, old_x, old_y) = self.pointers[index];
        if self.pointers.len() == 1 {
            self.travel += (x - old_x).abs() + (y - old_y).abs();
            self.pointers[index] = (event.pointer_id(), x, y);
            if self.travel > DRAG_SLOP {
                self.grab();
                self.view.pan(x - old_x, y - old_y);
                self.draw();
            }
            return;
        }
        // Two fingers: zoom by the change in their spread about their midpoint
        // and pan by the midpoint's movement.
        let other = self.pointers[usize::from(index == 0)];
        let spread = |ax: f64, ay: f64| (ax - other.1).hypot(ay - other.2);
        let (before, after) = (spread(old_x, old_y), spread(x, y));
        self.pointers[index] = (event.pointer_id(), x, y);
        self.grab();
        let (left, top) = self.origin();
        let (mid_x, mid_y) = ((x + other.1) / 2.0 - left, (y + other.2) / 2.0 - top);
        self.view.pan((x - old_x) / 2.0, (y - old_y) / 2.0);
        if before > 0.0 && after > 0.0 {
            self.view.zoom(after / before, mid_x, mid_y);
        }
        self.draw();
    }
    fn release(&mut self, event: &web_sys::PointerEvent) {
        self.pointers.retain(|(id, ..)| *id != event.pointer_id());
        if self.pointers.is_empty() {
            let _ = self.root.class_list().remove_1("dragging");
        }
    }
    fn wheel(&mut self, event: &web_sys::WheelEvent) {
        event.prevent_default();
        let (left, top) = self.origin();
        let factor = wheel_factor(event.delta_y(), event.delta_mode(), event.ctrl_key(), self.view.height);
        self.view.zoom(factor, event.client_x() as f64 - left, event.client_y() as f64 - top);
        self.draw();
    }
    fn command(&mut self, command: &str) -> bool {
        match command {
            "in" | "+" | "=" => self.view.zoom_centre(STEP),
            "out" | "-" | "_" => self.view.zoom_centre(1.0 / STEP),
            "fit" | "0" => self.view.fit(),
            "ArrowLeft" => self.view.pan(NUDGE, 0.0),
            "ArrowRight" => self.view.pan(-NUDGE, 0.0),
            "ArrowUp" => self.view.pan(0.0, NUDGE),
            "ArrowDown" => self.view.pan(0.0, -NUDGE),
            _ => return false,
        }
        self.draw();
        true
    }
}

fn listen<E: JsCast + 'static>(target: &web_sys::EventTarget, name: &str, capture: bool, canvas: &Rc<RefCell<Canvas>>, handle: fn(&mut Canvas, &E)) -> Result<(), JsValue> {
    let canvas = canvas.clone();
    let callback = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        if let Ok(mut canvas) = canvas.try_borrow_mut() {
            handle(&mut canvas, event.unchecked_ref());
        }
    });
    let options = web_sys::AddEventListenerOptions::new();
    options.set_capture(capture);
    options.set_passive(false);
    target.add_event_listener_with_callback_and_add_event_listener_options(name, callback.as_ref().unchecked_ref(), &options)?;
    callback.forget();
    Ok(())
}

fn attach_one(root: web_sys::HtmlElement) -> Result<(), JsValue> {
    let Some(svg) = root.query_selector("svg")? else { return Ok(()) };
    let Some(bounds) = svg.get_attribute("viewBox").as_deref().and_then(Rect::parse) else { return Ok(()) };
    let view = View::new(bounds, root.client_width() as f64, root.client_height() as f64);
    if let Some(controls) = root.query_selector(".canvas-controls")? {
        controls.remove_attribute("hidden")?;
    }
    root.class_list().add_1("live")?;
    let canvas = Rc::new(RefCell::new(Canvas { root: root.clone(), svg, view, pointers: Vec::new(), travel: 0.0, dragged: false }));
    canvas.borrow().draw();
    let target: &web_sys::EventTarget = root.as_ref();
    listen(target, "pointerdown", false, &canvas, Canvas::press)?;
    listen(target, "pointermove", false, &canvas, Canvas::drag)?;
    listen(target, "pointerup", false, &canvas, Canvas::release)?;
    listen(target, "pointercancel", false, &canvas, Canvas::release)?;
    listen(target, "wheel", false, &canvas, Canvas::wheel)?;
    listen(target, "dragstart", false, &canvas, |_, event: &web_sys::Event| event.prevent_default())?;
    // A drag that started on a link must not follow it.
    listen(target, "click", true, &canvas, |canvas, event: &web_sys::MouseEvent| {
        if std::mem::take(&mut canvas.dragged) {
            event.prevent_default();
            event.stop_propagation();
        }
    })?;
    listen(target, "click", false, &canvas, |canvas, event: &web_sys::MouseEvent| {
        let button = event.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()).and_then(|t| t.closest("[data-zoom]").ok().flatten());
        if let Some(command) = button.and_then(|b| b.get_attribute("data-zoom")) {
            canvas.command(&command);
        }
    })?;
    listen(target, "keydown", false, &canvas, |canvas, event: &web_sys::KeyboardEvent| {
        if !event.ctrl_key() && !event.meta_key() && !event.alt_key() && canvas.command(&event.key()) {
            event.prevent_default();
        }
    })?;
    let resized = canvas.clone();
    let observer = Closure::<dyn FnMut()>::new(move || {
        if let Ok(mut canvas) = resized.try_borrow_mut() {
            let (width, height) = (canvas.root.client_width() as f64, canvas.root.client_height() as f64);
            canvas.view.resize(width, height);
            canvas.draw();
        }
    });
    web_sys::ResizeObserver::new(observer.as_ref().unchecked_ref())?.observe(&root);
    observer.forget();
    Ok(())
}

/// Attaches every `[data-canvas]` drawing on the page; false when there are none.
pub fn attach(document: &web_sys::Document) -> Result<bool, JsValue> {
    let roots = document.query_selector_all("[data-canvas]")?;
    for index in 0..roots.length() {
        if let Some(root) = roots.item(index).and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok()) {
            attach_one(root)?;
        }
    }
    Ok(roots.length() > 0)
}

#[cfg(test)]
mod tests;
