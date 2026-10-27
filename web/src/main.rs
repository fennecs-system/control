use wasm_bindgen::{closure::Closure, JsCast, JsValue};

fn main() -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| JsValue::from_str("No document"))?;
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
