use std::{cell::Cell, rc::Rc};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};

const COMMANDS: [&str; 6] = ["/improve", "/improve off", "/steer", "/btw", "/spawn", "/cancel"];

fn completions(value: &str) -> Vec<usize> {
    if !value.starts_with('/') || value.contains('\n') {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .enumerate()
        .filter_map(|(index, command)| command.starts_with(value).then_some(index))
        .collect()
}

fn command_length(value: &str) -> usize {
    for command in ["/steer", "/btw", "/spawn", "/cancel"] {
        if value.strip_prefix(command).is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace)) {
            return command.len();
        }
    }
    let Some(rest) = value.strip_prefix("/improve") else {
        return 0;
    };
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return 0;
    }
    for suffix in [" off", " on"] {
        if rest
            .strip_prefix(suffix)
            .is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
        {
            return "/improve".len() + suffix.len();
        }
    }
    "/improve".len()
}

struct Composer {
    input: web_sys::HtmlTextAreaElement,
    mirror: web_sys::Element,
    document: web_sys::Document,
    menu: Option<web_sys::Element>,
    options: Vec<web_sys::Element>,
    selected: Cell<Option<usize>>,
    dismissed: Cell<bool>,
}

impl Composer {
    fn refresh(&self) -> Result<(), JsValue> {
        let value = self.input.value();
        self.mirror.set_text_content(None);
        let length = command_length(&value);
        if length > 0 {
            let command = self.document.create_element("span")?;
            command.set_attribute("class", "chat-command")?;
            command.set_text_content(Some(&value[..length]));
            self.mirror.append_child(&command)?;
        }
        let text = self.document.create_element("span")?;
        // A final character keeps a trailing newline aligned with the textarea.
        text.set_text_content(Some(&format!("{}\u{200b}", &value[length..])));
        self.mirror.append_child(&text)?;
        self.mirror.set_scroll_top(self.input.scroll_top());
        self.mirror.set_scroll_left(self.input.scroll_left());
        let matches = self.completions(&value);
        let visible = self.menu.is_some() && !self.dismissed.get() && !matches.is_empty();
        if let Some(menu) = &self.menu {
            menu.toggle_attribute_with_force("hidden", !visible)?;
            self.input
                .set_attribute("aria-expanded", if visible { "true" } else { "false" })?;
        }
        for (index, option) in self.options.iter().enumerate() {
            option.toggle_attribute_with_force("hidden", !matches.contains(&index))?;
            option.set_attribute(
                "aria-selected",
                if self.selected.get() == Some(index) {
                    "true"
                } else {
                    "false"
                },
            )?;
        }
        if visible {
            if let Some(index) = self.selected.get() {
                self.input
                    .set_attribute("aria-activedescendant", &self.options[index].id())?;
            } else {
                self.input.remove_attribute("aria-activedescendant")?;
            }
        } else {
            self.input.remove_attribute("aria-activedescendant")?;
        }
        Ok(())
    }

    fn complete(&self, index: usize) {
        self.input.set_value(COMMANDS[index]);
        self.selected.set(None);
        self.dismissed.set(true);
        let _ = self.input.focus();
        let end = self.input.value().encode_utf16().count() as u32;
        let _ = self.input.set_selection_range(end, end);
        let _ = self.refresh();
    }

    fn completions(&self, value: &str) -> Vec<usize> {
        completions(value).into_iter().filter(|index| self.options[*index].is_connected()).collect()
    }
}

fn attach_composer(
    document: &web_sys::Document,
    input: web_sys::HtmlTextAreaElement,
) -> Result<(), JsValue> {
    let wrapper = document.create_element("div")?;
    wrapper.set_attribute("class", "composer-input")?;
    input
        .parent_node()
        .ok_or_else(|| JsValue::from_str("No composer"))?
        .insert_before(&wrapper, Some(&input))?;
    let mirror = document.create_element("pre")?;
    mirror.set_attribute("class", "composer-highlight")?;
    mirror.set_attribute("aria-hidden", "true")?;
    wrapper.append_child(&mirror)?;
    wrapper.append_child(&input)?;
    let menu = document.get_element_by_id("chat-commands");
    let options = ["command-improve", "command-improve-off", "command-steer", "command-btw", "command-spawn", "command-cancel"]
        .into_iter()
        .map(|id| document.get_element_by_id(id).map(Ok).unwrap_or_else(|| document.create_element("button")))
        .collect::<Result<Vec<_>, _>>()?;
    if menu.is_some() {
        input.set_attribute("aria-autocomplete", "list")?;
        input.set_attribute("aria-controls", "chat-commands")?;
        input.set_attribute("aria-expanded", "false")?;
    }
    let composer = Rc::new(Composer {
        input: input.clone(),
        mirror,
        document: document.clone(),
        menu,
        options,
        selected: Cell::new(None),
        dismissed: Cell::new(false),
    });
    composer.refresh()?;
    let target = composer.clone();
    let changed = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        target.selected.set(None);
        target.dismissed.set(false);
        let _ = target.refresh();
    });
    input.add_event_listener_with_callback("input", changed.as_ref().unchecked_ref())?;
    changed.forget();
    let target = composer.clone();
    let scrolled = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        target.mirror.set_scroll_top(target.input.scroll_top());
        target.mirror.set_scroll_left(target.input.scroll_left());
    });
    input.add_event_listener_with_callback("scroll", scrolled.as_ref().unchecked_ref())?;
    scrolled.forget();
    for (index, option) in composer.options.iter().enumerate() {
        let target = composer.clone();
        let clicked = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| target.complete(index));
        option.add_event_listener_with_callback("click", clicked.as_ref().unchecked_ref())?;
        clicked.forget();
    }
    let target = composer;
    let handler =
        Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |event: web_sys::KeyboardEvent| {
            if event.is_composing()
                || event.key_code() == 229
                || event.default_prevented()
                || event.ctrl_key()
                || event.alt_key()
                || event.meta_key()
            {
                return;
            }
            let value = target.input.value();
            let matches = target.completions(&value);
            let visible = target.menu.is_some() && !target.dismissed.get() && !matches.is_empty();
            if visible && !event.shift_key() {
                match event.key().as_str() {
                    "Escape" => {
                        event.prevent_default();
                        target.dismissed.set(true);
                        let _ = target.refresh();
                        return;
                    }
                    "ArrowDown" | "ArrowUp" => {
                        event.prevent_default();
                        let position = target.selected.get().and_then(|index| {
                            matches.iter().position(|candidate| *candidate == index)
                        });
                        let next = match (event.key().as_str(), position) {
                            ("ArrowUp", Some(position)) => {
                                (position + matches.len() - 1) % matches.len()
                            }
                            ("ArrowUp", None) => matches.len() - 1,
                            (_, Some(position)) => (position + 1) % matches.len(),
                            _ => 0,
                        };
                        target.selected.set(Some(matches[next]));
                        let _ = target.refresh();
                        return;
                    }
                    "Tab" | "Enter"
                        if event.key() == "Tab"
                            || target.selected.get().is_some()
                            || !COMMANDS.contains(&value.trim_end()) =>
                    {
                        event.prevent_default();
                        target.complete(target.selected.get().unwrap_or(matches[0]));
                        return;
                    }
                    _ => {}
                }
            }
            if event.key() == "Enter" && !event.shift_key() {
                event.prevent_default();
                if !event.repeat() && !value.trim().is_empty() {
                    if let Some(form) = target.input.form() {
                        let _ = form.request_submit();
                    }
                }
            }
        });
    input.add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref())?;
    input.set_attribute("data-enter-sends", "true")?;
    input.set_attribute("aria-keyshortcuts", "Enter")?;
    handler.forget();
    Ok(())
}

fn main() -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| JsValue::from_str("No document"))?;
    if let Some(input) = document.query_selector(".conversation-composer textarea[name=message]")? {
        attach_composer(&document, input.dyn_into()?)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_and_highlighting_keep_arguments_and_regular_chat_distinct() {
        assert_eq!(completions("/"), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(completions("/imp"), vec![0, 1]);
        assert_eq!(completions("/improve o"), vec![1]);
        assert!(completions("/improve change the UI").is_empty());
        assert!(completions("ordinary chat").is_empty());
        assert!(completions("/improve\n").is_empty());
        assert_eq!(command_length("/improve change the UI"), 8);
        assert_eq!(command_length("/improve off"), 12);
        assert_eq!(command_length("/improve on"), 11);
        assert_eq!(command_length("/improvements"), 0);
        assert_eq!(command_length("ordinary chat"), 0);
        assert_eq!(completions("/bt"), vec![3]);
        assert_eq!(command_length("/btw <script>"), 4);
        assert_eq!(command_length("/steer change approach"), 6);
        assert_eq!(command_length("/steering"), 0);
    }
}
