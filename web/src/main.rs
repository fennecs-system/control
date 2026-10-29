use wasm_bindgen::{closure::Closure, JsCast, JsValue};

fn main() -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| JsValue::from_str("No document"))?;
    if let Some(input) = document.query_selector(".conversation-composer textarea[name=message]")? {
        let input: web_sys::HtmlTextAreaElement = input.dyn_into()?;
        let composer = input.clone();
        let handler = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
            move |event: web_sys::KeyboardEvent| {
                if event.key() != "Enter"
                    || event.shift_key()
                    || event.ctrl_key()
                    || event.alt_key()
                    || event.meta_key()
                    || event.is_composing()
                    || event.key_code() == 229
                    || event.default_prevented()
                {
                    return;
                }
                event.prevent_default();
                if event.repeat() || composer.value().trim().is_empty() {
                    return;
                }
                if let Some(form) = composer.form() {
                    let _ = form.request_submit();
                }
            },
        );
        input.add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref())?;
        input.set_attribute("data-enter-sends", "true")?;
        input.set_attribute("aria-keyshortcuts", "Enter")?;
        handler.forget();
        return Ok(());
    }
    let output = document
        .get_element_by_id("output")
        .ok_or_else(|| JsValue::from_str("No output surface"))?;
    let category = if web_sys::window()
        .and_then(|w| w.location().search().ok())
        .is_some_and(|query| query == "?lens=research")
    {
        "research"
    } else {
        "engineering"
    };
    output.set_text_content(Some(&control_context::inspect(
        category,
        &["context.inspect", "knowledge.propose"],
    )));
    for lens in ["engineering", "research"] {
        let button = document
            .get_element_by_id(lens)
            .ok_or_else(|| JsValue::from_str("No lens control"))?;
        let output = output.clone();
        let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            output.set_text_content(Some(&control_context::inspect(
                lens,
                &["context.inspect", "knowledge.propose"],
            )));
        });
        button.add_event_listener_with_callback("click", handler.as_ref().unchecked_ref())?;
        handler.forget();
    }
    Ok(())
}
