use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};

mod canvas;
use control_context::commands::{self, DEFINITIONS as COMMANDS};

fn completions(value: &str) -> Vec<usize> {
    commands::completions(value)
}
fn command_length(value: &str) -> usize {
    commands::parse(value)
        .filter(|c| c.highlight.start == 0)
        .map_or(0, |c| c.highlight.end)
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
        self.input
            .set_custom_validity(if control_context::limits::message_too_long(&value) {
                control_context::limits::CHAT_MESSAGE_ERROR
            } else {
                ""
            });
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
        self.input.set_value(COMMANDS[index].text);
        self.selected.set(None);
        self.dismissed.set(true);
        let _ = self.input.focus();
        let end = self.input.value().encode_utf16().count() as u32;
        let _ = self.input.set_selection_range(end, end);
        let _ = self.refresh();
    }

    fn completions(&self, value: &str) -> Vec<usize> {
        completions(value)
            .into_iter()
            .filter(|index| self.options[*index].is_connected())
            .collect()
    }
}

fn attach_composer(document: &web_sys::Document, input: web_sys::HtmlTextAreaElement) -> Result<(), JsValue> {
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
    let options = COMMANDS
        .iter()
        .map(|command| format!("command-{}", command.id))
        .map(|id| {
            document
                .get_element_by_id(&id)
                .map(Ok)
                .unwrap_or_else(|| document.create_element("button"))
        })
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
    attach_submission(&composer)?;
    let target = composer;
    let handler = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |event: web_sys::KeyboardEvent| {
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
                    let position = target
                        .selected
                        .get()
                        .and_then(|index| matches.iter().position(|candidate| *candidate == index));
                    let next = match (event.key().as_str(), position) {
                        ("ArrowUp", Some(position)) => (position + matches.len() - 1) % matches.len(),
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
                        || !COMMANDS.iter().any(|command| command.text == value.trim_end()) =>
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
            if !event.repeat()
                && !value.trim().is_empty()
                && let Some(form) = target.input.form()
            {
                let _ = form.request_submit();
            }
        }
    });
    input.add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref())?;
    input.set_attribute("data-enter-sends", "true")?;
    input.set_attribute("aria-keyshortcuts", "Enter")?;
    handler.forget();
    Ok(())
}

fn attach_submission(composer: &Rc<Composer>) -> Result<(), JsValue> {
    let Some(form) = composer.input.form().filter(|form| form.id() == "session-message") else {
        return Ok(());
    };
    let error = composer.document.create_element("p")?;
    error.set_attribute("role", "alert")?;
    error.set_attribute("hidden", "")?;
    form.append_child(&error)?;
    let button = form.query_selector(".send-message")?;
    let busy = Rc::new(Cell::new(false));
    let target = composer.clone();
    let submitted_form = form.clone();
    let submit = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        if target.input.value().trim() == "/open" {
            return;
        }
        if busy.get() {
            event.prevent_default();
            return;
        }
        // Preserve native form fallback if browser APIs cannot initialize.
        let prepare = || -> Result<_, JsValue> {
            let data = web_sys::FormData::new_with_form(&submitted_form)?;
            let params = web_sys::UrlSearchParams::new()?;
            for entry in js_sys::try_iter(&data)?.ok_or_else(|| JsValue::from_str("No form iterator"))? {
                let pair = js_sys::Array::from(&entry?);
                if let (Some(key), Some(value)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
                    params.append(&key, &value);
                }
            }
            let request = web_sys::XmlHttpRequest::new()?;
            request.open("POST", &submitted_form.action())?;
            request.set_request_header("Accept", "application/json")?;
            request.set_request_header("Content-Type", "application/x-www-form-urlencoded")?;
            request.set_timeout(15000);
            Ok((request, String::from(params.to_string())))
        };
        let Ok((request, payload)) = prepare() else {
            return;
        };
        event.prevent_default();
        busy.set(true);
        let _ = error.set_attribute("hidden", "");
        if let Some(button) = &button {
            let _ = button.set_attribute("disabled", "");
        }
        let response = request.clone();
        let done = busy.clone();
        let target = target.clone();
        let error = error.clone();
        let completed_button = button.clone();
        let send_error = error.clone();
        let submitted = target.input.value();
        let finished = Closure::<dyn FnMut(web_sys::Event)>::once(move |_| {
            done.set(false);
            if let Some(button) = &completed_button {
                let _ = button.remove_attribute("disabled");
            }
            let value = response
                .response_text()
                .ok()
                .flatten()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
            if response.status().ok() == Some(200) && value.as_ref().is_some_and(|v| v["title"].is_string()) {
                let value = value.unwrap();
                if target.input.value() == submitted {
                    target.input.set_value("");
                }
                target.selected.set(None);
                target.dismissed.set(true);
                let _ = target.refresh();
                if let Ok(Some(title)) = target.document.query_selector(".conversation-heading h2") {
                    title.set_text_content(value["title"].as_str());
                }
                if let Ok(Some(heading)) = target.document.query_selector(".conversation-heading") {
                    if let Ok(Some(mode)) = heading.query_selector(".active-command") {
                        mode.remove();
                    }
                    if value["improve"] == true
                        && let Ok(mode) = target.document.create_element("span")
                    {
                        let _ = mode.set_attribute("class", "chat-command active-command");
                        mode.set_text_content(Some("/improve"));
                        let _ = heading.append_child(&mode);
                    }
                }
                if let Ok(Some(frame)) = target.document.query_selector("iframe.chat-transcript")
                    && let Ok(frame) = frame.dyn_into::<web_sys::HtmlIFrameElement>()
                    && let Some(doc) = frame.content_document()
                    && let Some(scroller) = doc.scrolling_element()
                {
                    scroller.set_scroll_top(scroller.scroll_height());
                }
                if let Ok(event) = web_sys::Event::new("control-transcript-sent") {
                    let _ = target.document.dispatch_event(&event);
                }
                let _ = target.input.focus();
            } else {
                let message = value
                    .as_ref()
                    .and_then(|value| value["error"].as_str())
                    .unwrap_or("Could not confirm delivery. Check the conversation before sending again.");
                error.set_text_content(Some(message));
                let _ = error.remove_attribute("hidden");
            }
        });
        request.set_onloadend(Some(finished.as_ref().unchecked_ref()));
        if request.send_with_opt_str(Some(&payload)).is_err() {
            busy.set(false);
            if let Some(button) = &button {
                let _ = button.remove_attribute("disabled");
            }
            send_error.set_text_content(Some(
                "Could not confirm delivery. Check the conversation before sending again.",
            ));
            let _ = send_error.remove_attribute("hidden");
        }
        finished.forget();
    });
    form.add_event_listener_with_callback("submit", submit.as_ref().unchecked_ref())?;
    submit.forget();
    Ok(())
}

// Stable root identities retain old messages and interactive forms. Only changed
// streaming blocks are replaced; an append never rebuilds the preceding history.
fn keyed_nodes(body: &web_sys::HtmlElement) -> Vec<(String, web_sys::Node)> {
    let mut counts = std::collections::HashMap::<String, usize>::new();
    let nodes = body.child_nodes();
    (0..nodes.length())
        .filter_map(|index| nodes.item(index))
        .map(|node| {
            // `instanceof Element` fails across iframe realms in Chromium. The DOM
            // node type is realm-independent; this cast follows that explicit check.
            let base = if node.node_type() == web_sys::Node::ELEMENT_NODE {
                let element = node.unchecked_ref::<web_sys::Element>();
                if let Some(key) = element.get_attribute("data-transcript-key") {
                    format!("event:{key}")
                } else if !element.id().is_empty() {
                    format!("id:{}", element.id())
                } else {
                    format!(
                        "{}:{}",
                        element.tag_name(),
                        element.get_attribute("aria-label").unwrap_or_default()
                    )
                }
            } else {
                format!("node:{}", node.node_type())
            };
            let count = counts.entry(base.clone()).or_default();
            let key = format!("{base}:{count}");
            *count += 1;
            (key, node)
        })
        .collect()
}
fn reconcile(body: &web_sys::HtmlElement, new_body: &web_sys::HtmlElement) -> Result<(), JsValue> {
    let old = keyed_nodes(body);
    let wanted = keyed_nodes(new_body);
    let keys: std::collections::HashSet<_> = wanted.iter().map(|(key, _)| key.as_str()).collect();
    for (key, node) in &old {
        if !keys.contains(key.as_str()) {
            body.remove_child(node)?;
        }
    }
    let old: std::collections::HashMap<_, _> = old.into_iter().collect();
    let mut cursor = body.first_child();
    for (key, incoming) in wanted {
        let node = if let Some(existing) = old.get(&key) {
            // An older poll must not reset an in-flight decision or resurrect
            // its buttons after the server has acknowledged it. Once the port
            // leaves the next snapshot, the normal removal above still applies.
            let local_decision = existing.node_type() == web_sys::Node::ELEMENT_NODE && {
                let element = existing.unchecked_ref::<web_sys::Element>();
                element.matches(".port-request")?
                    && (element.has_attribute("data-port-decided")
                        || element.query_selector("form[data-submitting]")?.is_some())
            };
            if local_decision || existing.is_equal_node(Some(&incoming)) {
                existing.clone()
            } else {
                body.replace_child(&incoming, existing)?;
                if cursor.as_ref().is_some_and(|n| n.is_same_node(Some(existing))) {
                    cursor = Some(incoming.clone());
                }
                incoming
            }
        } else {
            incoming
        };
        if cursor.as_ref().is_some_and(|n| n.is_same_node(Some(&node))) {
            cursor = node.next_sibling();
        } else {
            body.insert_before(&node, cursor.as_ref())?;
        }
    }
    Ok(())
}

// The parent owns updates: browser-managed iframe refreshes can stall in Chromium
// and visibly reload in Firefox. Neither the frame nor the composer is navigated.
fn follow_transcript(frame: &web_sys::HtmlIFrameElement, following: Rc<Cell<bool>>) -> Result<(), JsValue> {
    let Some(doc) = frame.content_document() else {
        return Ok(());
    };
    let Some(scroller) = doc.scrolling_element() else {
        return Ok(());
    };
    let target = scroller.clone();
    let state = following.clone();
    let scroll = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        state.set(target.scroll_height() - target.client_height() - target.scroll_top() < 80);
    });
    doc.add_event_listener_with_callback("scroll", scroll.as_ref().unchecked_ref())?;
    scroll.forget();
    let resize = Closure::<dyn FnMut()>::new(move || {
        if following.get() {
            scroller.set_scroll_top(scroller.scroll_height());
        }
    });
    let observer = web_sys::ResizeObserver::new(resize.as_ref().unchecked_ref())?;
    if let Some(body) = doc.body() {
        observer.observe(&body);
    }
    // Keep the observer alive for this document's lifetime; replacing the frame
    // document on navigation releases its observed nodes with the page.
    std::mem::forget(observer);
    resize.forget();
    Ok(())
}
fn attach_port_approvals(frame: &web_sys::HtmlIFrameElement) -> Result<(), JsValue> {
    let Some(document) = frame.content_document() else {
        return Ok(());
    };
    let Some(root) = document.document_element() else {
        return Ok(());
    };
    if root.has_attribute("data-port-handler") {
        return Ok(());
    }
    root.set_attribute("data-port-handler", "")?;
    let submit = Closure::<dyn FnMut(web_sys::SubmitEvent)>::new(move |event: web_sys::SubmitEvent| {
        // Submit targets are forms. Use a realm-independent tag check rather
        // than instanceof, which rejects iframe forms in Chromium.
        let Some(form) = event
            .target()
            .map(|target| target.unchecked_into::<web_sys::HtmlFormElement>())
        else {
            return;
        };
        let submitter = event.submitter().and_then(|button| button.get_attribute("name"));
        let Some(action) = form
            .get_attribute("action")
            .filter(|action| decision_action(action, submitter.as_deref()))
        else {
            return;
        };
        if form.tag_name() != "FORM" {
            return;
        }
        if form.has_attribute("data-submitting") {
            event.prevent_default();
            return;
        }
        let prepare = || -> Result<_, JsValue> {
            let params = web_sys::UrlSearchParams::new()?;
            let data = web_sys::FormData::new_with_form(&form)?;
            for entry in js_sys::try_iter(&data)?.ok_or_else(|| JsValue::from_str("No form iterator"))? {
                let pair = js_sys::Array::from(&entry?);
                if let (Some(key), Some(value)) = (pair.get(0).as_string(), pair.get(1).as_string()) {
                    params.append(&key, &value);
                }
            }
            if let Some(button) = event.submitter()
                && let Some(name) = button.get_attribute("name")
            {
                params.append(&name, &button.get_attribute("value").unwrap_or_default());
            }
            params.append("response", "json");
            let request = web_sys::XmlHttpRequest::new()?;
            request.open("POST", &action)?;
            request.set_request_header("Content-Type", "application/x-www-form-urlencoded")?;
            request.set_timeout(15000);
            Ok((request, String::from(params.to_string())))
        };
        let Ok((request, payload)) = prepare() else {
            return;
        };
        event.prevent_default();
        let _ = form.set_attribute("data-submitting", "");
        let _ = form.set_attribute("aria-busy", "true");
        if let Ok(Some(error)) = form.query_selector("[role=alert]") {
            let _ = error.set_attribute("hidden", "");
        }
        let controls = form
            .query_selector_all("button, input, select")
            .ok()
            .map(|nodes| {
                (0..nodes.length())
                    .filter_map(|index| nodes.item(index))
                    .map(|node| node.unchecked_into::<web_sys::Element>())
                    .filter(|element| !element.has_attribute("disabled"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for control in &controls {
            let _ = control.set_attribute("disabled", "");
        }
        let button = event.submitter().map(|button| {
            let label = button.text_content().unwrap_or_default();
            button.set_text_content(Some(match button.get_attribute("name").as_deref() {
                Some("revoke" | "deny") => "Denying…",
                Some("dismiss") => "Dismissing…",
                // Folder approval is accepted at once; the refreshed panel shows the import.
                Some("approve") => "Importing…",
                _ => "Allowing…",
            }));
            (button, label)
        });
        let response = request.clone();
        let failed_form = form.clone();
        let failed_controls = controls.clone();
        let failed_button = button.clone();
        let finished = Closure::<dyn FnMut(web_sys::Event)>::once(move |_| {
            let value = response
                .response_text()
                .ok()
                .flatten()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
            if response.status().ok() == Some(200) && value.as_ref().is_some_and(|value| value["ok"] == true) {
                if let Ok(Some(prompt)) = form.closest(".port-request") {
                    let _ = prompt.set_attribute("data-port-decided", "");
                    let _ = prompt.set_attribute("hidden", "");
                }
                if let Some(parent) = web_sys::window().and_then(|window| window.document())
                    && let Ok(event) = web_sys::Event::new("control-transcript-refresh")
                {
                    let _ = parent.dispatch_event(&event);
                }
            } else {
                restore_port_form(&form, &controls, button.as_ref());
                if let Ok(Some(error)) = form.query_selector("[role=alert]") {
                    error.set_text_content(Some(
                        value
                            .as_ref()
                            .and_then(|value| value["error"].as_str())
                            .unwrap_or("Could not confirm the decision. Check status before retrying."),
                    ));
                    let _ = error.remove_attribute("hidden");
                }
            }
        });
        request.set_onloadend(Some(finished.as_ref().unchecked_ref()));
        if request.send_with_opt_str(Some(&payload)).is_err() {
            restore_port_form(&failed_form, &failed_controls, failed_button.as_ref());
            if let Ok(Some(error)) = failed_form.query_selector("[role=alert]") {
                error.set_text_content(Some("Could not confirm the decision. Check status before retrying."));
                let _ = error.remove_attribute("hidden");
            }
        }
        finished.forget();
    });
    document.add_event_listener_with_callback("submit", submit.as_ref().unchecked_ref())?;
    submit.forget();
    Ok(())
}
/// Operator decisions posted without leaving the chat: port and bot requests,
/// and folder decisions (`/dm/ID/project` with its approve or dismiss button).
/// Changing the requested folder stays a normal form submission.
fn decision_action(action: &str, submitter: Option<&str>) -> bool {
    matches!(action, "/tools/ports" | "/tools/bots")
        || (matches!(submitter, Some("approve" | "dismiss"))
            && action
                .strip_prefix("/dm/")
                .and_then(|rest| rest.strip_suffix("/project"))
                .is_some_and(|id| !id.is_empty() && !id.contains('/')))
}
fn restore_port_form(
    form: &web_sys::HtmlFormElement,
    controls: &[web_sys::Element],
    button: Option<&(web_sys::HtmlElement, String)>,
) {
    let _ = form.remove_attribute("data-submitting");
    let _ = form.remove_attribute("aria-busy");
    for control in controls {
        let _ = control.remove_attribute("disabled");
    }
    if let Some((button, label)) = button {
        button.set_text_content(Some(label));
    }
}
fn attach_transcript(document: &web_sys::Document) -> Result<(), JsValue> {
    let frames = document.query_selector_all("iframe[data-live-chat]")?;
    for index in 0..frames.length() {
        if let Some(frame) = frames.item(index) {
            attach_chat_frame(document, frame.unchecked_into())?;
        }
    }
    Ok(())
}
fn update_request_count(document: &web_sys::Document, body: &web_sys::HtmlElement) {
    let Some(count) = body.get_attribute("data-request-count") else {
        return;
    };
    let Ok(Some(toggle)) = document.query_selector(".chat-requests") else {
        return;
    };
    let previous = toggle
        .get_attribute("data-count")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let current = count.parse::<usize>().unwrap_or(0);
    if let Ok(Some(label)) = toggle.query_selector(".request-count") {
        label.set_text_content(Some(&count));
    }
    if current > previous {
        let _ = toggle.set_attribute("open", "");
    }
    let _ = toggle.set_attribute("data-count", &count);
}
fn attach_chat_frame(document: &web_sys::Document, frame: web_sys::HtmlIFrameElement) -> Result<(), JsValue> {
    let transcript = frame.matches(".chat-transcript")?;
    let following = Rc::new(Cell::new(transcript));
    let loaded_frame = frame.clone();
    // A sent message always returns the reader to the end, like a chat client.
    let sent_following = following.clone();
    let sent_document = document.clone();
    let sent = Closure::<dyn FnMut()>::new(move || {
        sent_following.set(transcript);
        if let Ok(event) = web_sys::Event::new("control-transcript-refresh") {
            let _ = sent_document.dispatch_event(&event);
        }
    });
    document.add_event_listener_with_callback("control-transcript-sent", sent.as_ref().unchecked_ref())?;
    sent.forget();
    let loaded_following = following.clone();
    let parent_document = document.clone();
    let loaded = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        loaded_following.set(transcript);
        if transcript {
            let _ = follow_transcript(&loaded_frame, loaded_following.clone());
        }
        let _ = attach_port_approvals(&loaded_frame);
        if let Some(body) = loaded_frame.content_document().and_then(|d| d.body()) {
            update_request_count(&parent_document, &body);
        }
    });
    frame.add_event_listener_with_callback("load", loaded.as_ref().unchecked_ref())?;
    loaded.forget();
    if transcript {
        follow_transcript(&frame, following.clone())?;
    }
    attach_port_approvals(&frame)?;
    if let Some(body) = frame.content_document().and_then(|d| d.body()) {
        update_request_count(document, &body);
    }
    let url = frame
        .get_attribute("src")
        .unwrap_or_default()
        .split('#')
        .next()
        .unwrap_or_default()
        .to_owned();
    let busy = Rc::new(Cell::new(false));
    let pending = Rc::new(Cell::new(false));
    let previous = Rc::new(RefCell::new(String::new()));
    let parent_document = document.clone();
    let poll = Closure::<dyn FnMut()>::new(move || {
        if busy.replace(true) {
            pending.set(true);
            return;
        }
        let Ok(request) = web_sys::XmlHttpRequest::new() else {
            busy.set(false);
            return;
        };
        if request.open("GET", &url).is_err() {
            busy.set(false);
            return;
        }
        request.set_timeout(10_000);
        let response = request.clone();
        let frame = frame.clone();
        let following = following.clone();
        let previous = previous.clone();
        let completed = busy.clone();
        let pending = pending.clone();
        let parent_document = parent_document.clone();
        let finished = Closure::<dyn FnMut(web_sys::Event)>::once(move |_| {
            completed.set(false);
            if pending.replace(false)
                && let Ok(event) = web_sys::Event::new("control-transcript-refresh")
            {
                let _ = parent_document.dispatch_event(&event);
            }
            if response.status().ok() != Some(200) {
                let retry = Closure::<dyn FnMut()>::once(move || {
                    if let Ok(event) = web_sys::Event::new("control-transcript-refresh") {
                        let _ = parent_document.dispatch_event(&event);
                    }
                });
                let _ = web_sys::window()
                    .unwrap()
                    .set_timeout_with_callback_and_timeout_and_arguments_0(retry.as_ref().unchecked_ref(), 1000);
                retry.forget();
                return;
            }
            let Some(text) = response.response_text().ok().flatten() else {
                return;
            };
            if *previous.borrow() == text {
                return;
            }
            let Some(doc) = frame.content_document() else {
                return;
            };
            let Some(body) = doc.body() else {
                return;
            };
            let Ok(parser) = web_sys::DomParser::new() else {
                return;
            };
            let Ok(parsed) = parser.parse_from_string(&text, web_sys::SupportedType::TextHtml) else {
                return;
            };
            let Some(new_body) = parsed.body() else {
                return;
            };
            update_request_count(&parent_document, &new_body);
            let Some(scroller) = doc.scrolling_element() else {
                return;
            };
            let top = scroller.scroll_top();
            let at_end = following.get();
            let mut details = std::collections::HashMap::new();
            if let Ok(nodes) = body.query_selector_all("details") {
                for index in 0..nodes.length() {
                    if let Some(node) = nodes
                        .item(index)
                        .map(|n| n.unchecked_into::<web_sys::HtmlDetailsElement>())
                        && let Some(key) = node.get_attribute("data-transcript-key")
                    {
                        details.insert(key, node.open());
                    }
                }
            }
            if let Ok(nodes) = new_body.query_selector_all("details") {
                for index in 0..nodes.length() {
                    if let Some(node) = nodes
                        .item(index)
                        .map(|n| n.unchecked_into::<web_sys::HtmlDetailsElement>())
                        && let Some(open) = node
                            .get_attribute("data-transcript-key")
                            .and_then(|key| details.get(&key))
                    {
                        node.set_open(*open);
                    }
                }
            }
            if reconcile(&body, &new_body).is_err() {
                return;
            }
            scroller.set_scroll_top(if at_end { scroller.scroll_height() } else { top });
            // Layout can settle after reconciliation (fonts, details and frame
            // sizing). Follow again at the next paint, but respect scrollback.
            let callback = Closure::<dyn FnMut(f64)>::once(move |_| {
                if following.get() == at_end {
                    scroller.set_scroll_top(if at_end { scroller.scroll_height() } else { top });
                }
            });
            let _ = web_sys::window()
                .unwrap()
                .request_animation_frame(callback.as_ref().unchecked_ref());
            callback.forget();
            *previous.borrow_mut() = text;
        });
        request.set_onloadend(Some(finished.as_ref().unchecked_ref()));
        if request.send().is_err() {
            busy.set(false);
        }
        finished.forget();
    });
    document.add_event_listener_with_callback("control-transcript-refresh", poll.as_ref().unchecked_ref())?;
    poll.forget();
    Ok(())
}

// Refresh only the disclosed picker; keep its scroll and expanded branches.
fn attach_navigation(document: &web_sys::Document) -> Result<(), JsValue> {
    let Some(toggle) = document.query_selector(".conversation-toggle")? else {
        return Ok(());
    };
    // Presentation state only. The server renders the panel from this cookie,
    // so the first paint is already correct. Remember clicks before a link
    // changes pages.
    if let Ok(html) = document.clone().dyn_into::<web_sys::HtmlDocument>() {
        let target = toggle.clone();
        let remember = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            if event.type_() == "click"
                && event
                    .target()
                    .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                    .and_then(|element| element.closest("a[href]").ok().flatten())
                    .is_none()
            {
                return;
            }
            let state = if target.has_attribute("open") { "open" } else { "closed" };
            let _ = html.set_cookie(&format!(
                "control_conversations={state}; Path=/; Max-Age=31536000; SameSite=Strict"
            ));
        });
        for event in ["toggle", "click"] {
            toggle.add_event_listener_with_callback(event, remember.as_ref().unchecked_ref())?;
        }
        remember.forget();
    }
    let Some(frame) = document.query_selector("iframe.chat-transcript")? else {
        return Ok(());
    };
    let source = frame.get_attribute("src").unwrap_or_default();
    let Some(chat) = source.split('/').nth(2) else {
        return Ok(());
    };
    let url = format!("/dm/{chat}/conversations");
    let busy = Rc::new(Cell::new(false));
    let previous = Rc::new(RefCell::new(String::new()));
    let pending = Rc::new(Cell::new(false));
    let parent = document.clone();
    let target = toggle.clone();
    let poll = Rc::new(Closure::<dyn FnMut()>::new(move || {
        if !target.has_attribute("open") {
            return;
        }
        if busy.replace(true) {
            pending.set(true);
            return;
        }
        let Ok(request) = web_sys::XmlHttpRequest::new() else {
            busy.set(false);
            return;
        };
        if request.open("GET", &url).is_err() {
            busy.set(false);
            return;
        }
        request.set_timeout(10000);
        let response = request.clone();
        let target = target.clone();
        let completed = busy.clone();
        let previous = previous.clone();
        let pending = pending.clone();
        let parent = parent.clone();
        let done = Closure::<dyn FnMut(web_sys::Event)>::once(move |_| {
            completed.set(false);
            if pending.replace(false)
                && let Ok(event) = web_sys::Event::new("control-transcript-refresh")
            {
                let _ = parent.dispatch_event(&event);
            }
            if response.status().ok() != Some(200) {
                return;
            }
            let Some(text) = response.response_text().ok().flatten() else {
                return;
            };
            if *previous.borrow() == text {
                return;
            }
            let Ok(Some(aside)) = target.query_selector(".conversation-navigation") else {
                return;
            };
            let old_scroll = aside
                .query_selector("nav")
                .ok()
                .flatten()
                .map_or(0, |nav| nav.scroll_top());
            let mut expanded = std::collections::HashMap::new();
            if let Ok(nodes) = aside.query_selector_all("details[data-chat-branch]") {
                for index in 0..nodes.length() {
                    if let Some(node) = nodes
                        .item(index)
                        .and_then(|n| n.dyn_into::<web_sys::HtmlDetailsElement>().ok())
                        && let Some(id) = node.get_attribute("data-chat-branch")
                    {
                        expanded.insert(id, node.open());
                    }
                }
            }
            let Ok(parser) = web_sys::DomParser::new() else {
                return;
            };
            let Ok(parsed) = parser.parse_from_string(&text, web_sys::SupportedType::TextHtml) else {
                return;
            };
            let Ok(Some(new_aside)) = parsed.query_selector(".conversation-navigation") else {
                return;
            };
            if let Ok(nodes) = new_aside.query_selector_all("details[data-chat-branch]") {
                for index in 0..nodes.length() {
                    if let Some(node) = nodes
                        .item(index)
                        .and_then(|n| n.dyn_into::<web_sys::HtmlDetailsElement>().ok())
                        && let Some(open) = node.get_attribute("data-chat-branch").and_then(|id| expanded.get(&id))
                    {
                        node.set_open(*open);
                    }
                }
            }
            // The first poll can equal server-rendered content. Keep live links
            // and focus instead of replacing an unchanged tree during a click.
            if !aside.is_equal_node(Some(new_aside.unchecked_ref())) {
                aside.set_inner_html(&new_aside.inner_html());
                if let Ok(Some(nav)) = aside.query_selector("nav") {
                    nav.set_scroll_top(old_scroll);
                }
            }
            *previous.borrow_mut() = text;
        });
        request.set_onloadend(Some(done.as_ref().unchecked_ref()));
        if request.send().is_err() {
            busy.set(false);
        }
        done.forget();
    }));
    // State-change hints and disclosure use the same Rust callback.
    toggle.add_event_listener_with_callback("toggle", poll.as_ref().as_ref().unchecked_ref())?;
    document.add_event_listener_with_callback("control-transcript-refresh", poll.as_ref().as_ref().unchecked_ref())?;
    std::mem::forget(poll);
    Ok(())
}

/// Dispatched on the document whenever the page should re-read its
/// authoritative HTML: a state-change hint, a (re)connection, or recovery.
const REFRESH: &str = "control-transcript-refresh";
fn request_refresh(document: &web_sys::Document) {
    if let Ok(event) = web_sys::Event::new(REFRESH) {
        let _ = document.dispatch_event(&event);
    }
}

/// Milliseconds before reopening a live stream the browser gave up on, after
/// `failures` consecutive attempts: 1 s, doubling to at most 30 s.
fn reconnect_delay(failures: u32) -> i32 {
    1000i32.saturating_mul(1 << failures.min(5)).min(30_000)
}

/// The module that loads this client. It names the hashed WASM bundle, so it
/// changes exactly when a deploy changes the client, with or without a
/// recorded Git revision.
const CLIENT: &str = "/tool-window/chat-bootstrap.js";

/// Whether the server now serves a different client from the one this page
/// runs. An unknown first reading never counts as a change.
fn client_changed(loaded: Option<&str>, current: &str) -> bool {
    loaded.is_some_and(|loaded| loaded != current)
}

/// A stale page reloads itself only when no typing could be lost: nothing is
/// being edited and the composer, if any, holds no draft.
fn reload_is_quiet(editing: bool, draft: &str) -> bool {
    !editing && draft.trim().is_empty()
}

/// The page's single live connection and what it knows about the server.
struct Live {
    document: web_sys::Document,
    stream: RefCell<Option<web_sys::EventSource>>,
    /// Consecutive connections the browser abandoned (a non-200 answer such as
    /// `503 Control is stopping` while the server restarts ends EventSource's
    /// own retries for good).
    failures: Cell<u32>,
    /// The connection dropped since the last open: check the client on reopen.
    interrupted: Cell<bool>,
    /// The client bootstrap as first read by this page.
    client: RefCell<Option<String>>,
    /// The server was redeployed; reload once nothing would be lost.
    stale: Cell<bool>,
}

impl Live {
    fn open(self: &Rc<Self>) -> Result<(), JsValue> {
        let stream = web_sys::EventSource::new("/events")?;
        let live = self.clone();
        let changed = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            if live.stale.get() && live.reload_if_quiet() {
                return;
            }
            request_refresh(&live.document);
        });
        stream.add_event_listener_with_callback("message", changed.as_ref().unchecked_ref())?;
        changed.forget();
        let live = self.clone();
        let opened = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            live.failures.set(0);
            // Every connection starts with a hint, so the page re-reads its
            // state; after an interruption also learn whether the client changed.
            if live.interrupted.replace(false) {
                live.check_client();
            }
        });
        stream.add_event_listener_with_callback("open", opened.as_ref().unchecked_ref())?;
        opened.forget();
        let live = self.clone();
        let failed = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            live.interrupted.set(true);
            // While CONNECTING the browser retries by itself. A closed stream
            // is reopened here, quietly and with backoff, for the page's life.
            if !live.closed() {
                return;
            }
            let failures = live.failures.get();
            live.failures.set(failures.saturating_add(1));
            let again = live.clone();
            let retry = Closure::once_into_js(move || {
                let _ = again.open();
            });
            if let Some(window) = web_sys::window() {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                    retry.unchecked_ref(),
                    reconnect_delay(failures),
                );
            }
        });
        stream.add_event_listener_with_callback("error", failed.as_ref().unchecked_ref())?;
        failed.forget();
        if let Some(previous) = self.stream.replace(Some(stream)) {
            previous.close();
        }
        Ok(())
    }

    fn state(&self) -> Option<u16> {
        self.stream.borrow().as_ref().map(web_sys::EventSource::ready_state)
    }
    fn connected(&self) -> bool {
        self.state() == Some(web_sys::EventSource::OPEN)
    }
    fn closed(&self) -> bool {
        self.state().is_none_or(|state| state == web_sys::EventSource::CLOSED)
    }

    /// Reads the client bootstrap. The first reading is this page's client; a
    /// later different one (a redeploy with a new client) marks the page stale.
    fn check_client(self: &Rc<Self>) {
        let Ok(request) = web_sys::XmlHttpRequest::new() else {
            return;
        };
        if request.open("GET", CLIENT).is_err() {
            return;
        }
        request.set_timeout(10_000);
        let (response, live) = (request.clone(), self.clone());
        let finished = Closure::once_into_js(move |_: web_sys::Event| {
            if response.status().ok() != Some(200) {
                return;
            }
            let Some(current) = response.response_text().ok().flatten() else {
                return;
            };
            let loaded = live.client.borrow().clone();
            if loaded.is_none() {
                *live.client.borrow_mut() = Some(current);
                return;
            }
            if client_changed(loaded.as_deref(), &current) {
                live.stale.set(true);
                live.reload_if_quiet();
            }
        });
        request.set_onloadend(Some(finished.unchecked_ref()));
        let _ = request.send();
    }

    /// A page from an earlier deploy runs that deploy's client against the new
    /// server's routes and markup. Load the current page instead, unless the
    /// operator is typing; the next hint tries again.
    fn reload_if_quiet(&self) -> bool {
        let editing = self.document.active_element().is_some_and(|element| {
            matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
                || element
                    .dyn_ref::<web_sys::HtmlElement>()
                    .is_some_and(|element| element.is_content_editable())
        });
        let draft = self
            .document
            .query_selector(".conversation-composer textarea[name=message]")
            .ok()
            .flatten()
            .and_then(|input| input.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
            .map(|input| input.value())
            .unwrap_or_default();
        if !reload_is_quiet(editing, &draft) {
            return false;
        }
        web_sys::window().is_some_and(|window| window.location().reload().is_ok())
    }
}

// One authenticated, payload-free stream per page. Every (re)connection and
// hint refreshes the authoritative HTML; a slow recovery poll runs only while
// disconnected. A server restart can answer the browser's own retry with a
// non-200 status, which ends EventSource's retries for good, so a closed
// stream is reopened here; a redeployed client reloads a quiet page.
fn attach_updates(document: &web_sys::Document) -> Result<(), JsValue> {
    let live = Rc::new(Live {
        document: document.clone(),
        stream: RefCell::new(None),
        failures: Cell::new(0),
        interrupted: Cell::new(false),
        client: RefCell::new(None),
        stale: Cell::new(false),
    });
    live.check_client();
    live.open()?;
    let recovery = live.clone();
    let fallback = Closure::<dyn FnMut()>::new(move || {
        if recovery.connected() {
            return;
        }
        if recovery.stale.get() && recovery.reload_if_quiet() {
            return;
        }
        request_refresh(&recovery.document);
    });
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("No window"))?;
    window.set_interval_with_callback_and_timeout_and_arguments_0(fallback.as_ref().unchecked_ref(), 10_000)?;
    fallback.forget();
    // The page owns the connection. The browser closes/suspends it with the
    // document; returning through the back/forward cache reopens it.
    let restored = live.clone();
    let shown = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        if !restored.closed() {
            return;
        }
        restored.interrupted.set(true);
        let _ = restored.open();
    });
    window.add_event_listener_with_callback("pageshow", shown.as_ref().unchecked_ref())?;
    shown.forget();
    Ok(())
}

/// Pages that render from state first mark a `data-live-src` region; slow
/// engine reads arrive afterwards as HTML fragments keyed by `data-live`.
/// With `data-live-every` (seconds) the region is read again on that interval
/// while the page is visible, one request at a time; a hidden or closed page
/// reads nothing. A slot's `data-live-fallback` replaces the default text
/// shown when its fragment is missing.
///
/// A region marked `data-live-refresh` is itself rendered from state: on each
/// state-change hint from the page's live stream it is read again from the
/// page's own URL and replaced when it changed, keeping the usage already
/// shown in slots that remain. Fragments are read again only for new slots.
fn attach_live(document: &web_sys::Document) -> Result<bool, JsValue> {
    let Some(root) = document.query_selector("[data-live-src]")? else {
        return Ok(false);
    };
    let Some(url) = root.get_attribute("data-live-src") else {
        return Ok(false);
    };
    let every = root
        .get_attribute("data-live-every")
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|s| (1..=3600).contains(s));
    let refreshed = root.has_attribute("data-live-refresh").then(|| root.inner_html());
    let busy = Rc::new(Cell::new(false));
    let again = Rc::new(Cell::new(false));
    let target = document.clone();
    let this = Rc::new(RefCell::new(None::<std::rc::Weak<dyn Fn()>>));
    let (myself, region) = (this.clone(), root.clone());
    let read: Rc<dyn Fn()> = Rc::new(move || {
        let (this, root) = (myself.clone(), region.clone());
        if target.hidden() {
            return;
        }
        if busy.get() {
            again.set(true);
            return;
        }
        let Ok(request) = web_sys::XmlHttpRequest::new() else {
            return;
        };
        if request.open("GET", &url).is_err() {
            return;
        }
        request.set_timeout(15_000);
        busy.set(true);
        let (response, root, done, again, this) =
            (request.clone(), root.clone(), busy.clone(), again.clone(), this.clone());
        let finished = Closure::once_into_js(move |_: web_sys::Event| {
            done.set(false);
            // Slots added while this read was in flight need their own read.
            if again.replace(false)
                && let Some(read) = this.borrow().as_ref().and_then(std::rc::Weak::upgrade)
            {
                read();
            }
            let fragments = (response.status().ok() == Some(200))
                .then(|| response.response_text().ok().flatten())
                .flatten()
                .and_then(|text| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text).ok())
                .unwrap_or_default();
            let Ok(slots) = root.query_selector_all("[data-live]") else {
                return;
            };
            for index in 0..slots.length() {
                let Some(slot) = slots.item(index).map(|node| node.unchecked_into::<web_sys::Element>()) else {
                    continue;
                };
                match slot
                    .get_attribute("data-live")
                    .and_then(|id| fragments.get(&id)?.as_str().map(str::to_owned))
                {
                    Some(html) => slot.set_inner_html(&html),
                    None => match slot.get_attribute("data-live-fallback") {
                        Some(text) => slot.set_text_content(Some(&text)),
                        None => slot.set_inner_html("<p class='muted'>Usage unavailable.</p>"),
                    },
                }
            }
        });
        // Freed by the browser after its single call, so polling does not leak.
        request.set_onloadend(Some(finished.unchecked_ref()));
        if request.send().is_err() {
            busy.set(false);
        }
    });
    *this.borrow_mut() = Some(Rc::downgrade(&read));
    read();
    if let Some(previous) = refreshed {
        attach_region_refresh(document, root, previous, read.clone())?;
        attach_updates(document)?;
    }
    if let Some(seconds) = every {
        let tick = Closure::<dyn FnMut()>::new(move || read());
        web_sys::window()
            .ok_or_else(|| JsValue::from_str("No window"))?
            .set_interval_with_callback_and_timeout_and_arguments_0(tick.as_ref().unchecked_ref(), seconds * 1000)?;
        tick.forget();
    } else {
        // Held for the page's life by the refresh and timers that use it.
        std::mem::forget(read);
    }
    Ok(true)
}

/// Whether a refreshed region has `data-live` slots the shown one lacked, so
/// their fragments must be read.
fn slots_added(shown: &[String], incoming: &[String]) -> bool {
    incoming.iter().any(|id| !shown.contains(id))
}

fn slot_ids(root: &web_sys::Element) -> Vec<String> {
    root.query_selector_all("[data-live]")
        .map(|slots| {
            (0..slots.length())
                .filter_map(|index| {
                    slots
                        .item(index)?
                        .dyn_into::<web_sys::Element>()
                        .ok()?
                        .get_attribute("data-live")
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Re-reads a `data-live-refresh` region from the page's own URL on each
/// refresh hint, one request at a time; a hint during a read reads once more.
/// `previous` is the region as last rendered by the server.
fn attach_region_refresh(
    document: &web_sys::Document,
    root: web_sys::Element,
    previous: String,
    read: Rc<dyn Fn()>,
) -> Result<(), JsValue> {
    let busy = Rc::new(Cell::new(false));
    let pending = Rc::new(Cell::new(false));
    let previous = Rc::new(RefCell::new(previous));
    let target = document.clone();
    let refresh = Closure::<dyn FnMut()>::new(move || {
        if busy.replace(true) {
            pending.set(true);
            return;
        }
        let Some(location) = web_sys::window().map(|window| window.location()) else {
            busy.set(false);
            return;
        };
        let url = format!(
            "{}{}",
            location.pathname().unwrap_or_default(),
            location.search().unwrap_or_default()
        );
        let Ok(request) = web_sys::XmlHttpRequest::new() else {
            busy.set(false);
            return;
        };
        if request.open("GET", &url).is_err() {
            busy.set(false);
            return;
        }
        request.set_timeout(15_000);
        let (response, root, done, pending, previous, read, target) = (
            request.clone(),
            root.clone(),
            busy.clone(),
            pending.clone(),
            previous.clone(),
            read.clone(),
            target.clone(),
        );
        let finished = Closure::once_into_js(move |_: web_sys::Event| {
            done.set(false);
            if pending.replace(false) {
                request_refresh(&target);
            }
            if response.status().ok() != Some(200) {
                return;
            }
            let Some(text) = response.response_text().ok().flatten() else {
                return;
            };
            let Ok(parser) = web_sys::DomParser::new() else {
                return;
            };
            let Ok(parsed) = parser.parse_from_string(&text, web_sys::SupportedType::TextHtml) else {
                return;
            };
            let Ok(Some(incoming)) = parsed.query_selector("[data-live-src][data-live-refresh]") else {
                return;
            };
            let html = incoming.inner_html();
            if *previous.borrow() == html {
                return;
            }
            *previous.borrow_mut() = html;
            // Keep the usage already shown until the next fragment read.
            let shown = slot_ids(&root);
            if let Ok(slots) = incoming.query_selector_all("[data-live]") {
                for index in 0..slots.length() {
                    let Some(slot) = slots
                        .item(index)
                        .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                    else {
                        continue;
                    };
                    let Some(id) = slot.get_attribute("data-live") else {
                        continue;
                    };
                    let selector = format!("[data-live=\"{}\"]", id.replace(['\\', '"'], ""));
                    if let Ok(Some(current)) = root.query_selector(&selector) {
                        slot.set_inner_html(&current.inner_html());
                    }
                }
            }
            root.set_inner_html(&incoming.inner_html());
            if slots_added(&shown, &slot_ids(&root)) {
                read();
            }
        });
        request.set_onloadend(Some(finished.unchecked_ref()));
        if request.send().is_err() {
            busy.set(false);
        }
    });
    document.add_event_listener_with_callback(REFRESH, refresh.as_ref().unchecked_ref())?;
    refresh.forget();
    Ok(())
}

/// Whether a next-page slot whose top is at `top` (viewport pixels) should
/// load now: on screen, or within one screen of it.
fn near_viewport(top: f64, viewport: f64) -> bool {
    top < viewport * 2.0
}

/// Paged server lists end in a `[data-more]` slot that names the next page's
/// fragment and holds a plain link to the same page. While the slot nears the
/// viewport, or when its link is clicked, the fragment (rows followed by the
/// following slot, if any) replaces it. One request at a time; without the
/// client, the link loads the next page.
fn attach_more(document: &web_sys::Document) -> Result<bool, JsValue> {
    if document.query_selector("[data-more]")?.is_none() {
        return Ok(false);
    }
    let busy = Rc::new(Cell::new(false));
    let target = document.clone();
    let load = Rc::new(move |forced: bool| {
        if busy.get() {
            return;
        }
        let Ok(Some(slot)) = target.query_selector("[data-more]") else {
            return;
        };
        let viewport = web_sys::window()
            .and_then(|window| window.inner_height().ok())
            .and_then(|height| height.as_f64())
            .unwrap_or(800.0);
        if !forced && !near_viewport(slot.get_bounding_client_rect().top(), viewport) {
            return;
        }
        let Some(url) = slot.get_attribute("data-more") else {
            return;
        };
        let Ok(request) = web_sys::XmlHttpRequest::new() else {
            return;
        };
        if request.open("GET", &url).is_err() {
            return;
        }
        request.set_timeout(15_000);
        busy.set(true);
        let (response, done, page) = (request.clone(), busy.clone(), target.clone());
        let finished = Closure::once_into_js(move |_: web_sys::Event| {
            done.set(false);
            if response.status().ok() != Some(200) {
                return;
            }
            if let Some(text) = response.response_text().ok().flatten() {
                slot.set_outer_html(&text);
                // A tall screen may still show the next slot: check again.
                if let Ok(event) = web_sys::Event::new("scroll") {
                    let _ = page.dispatch_event(&event);
                }
            }
        });
        request.set_onloadend(Some(finished.unchecked_ref()));
        if request.send().is_err() {
            busy.set(false);
        }
    });
    let scrolled = load.clone();
    let scroll = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| scrolled(false));
    document.add_event_listener_with_callback("scroll", scroll.as_ref().unchecked_ref())?;
    scroll.forget();
    let clicked = load.clone();
    let click = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        let inside = event
            .target()
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| element.closest("[data-more] a").ok().flatten())
            .is_some();
        if inside {
            event.prevent_default();
            clicked(true);
        }
    });
    document.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    click.forget();
    load(false);
    Ok(true)
}

/// History rows are `<details data-turns=URL>` holding a `[data-turns-slot]`
/// with a plain link to the same turns as a page. The first time a row opens,
/// the fragment at URL replaces the slot; a failed read leaves the link and
/// tries again on the next opening. One request per row at a time.
fn attach_disclosures(document: &web_sys::Document) -> Result<bool, JsValue> {
    if document.query_selector("details[data-turns]")?.is_none() {
        return Ok(false);
    }
    let toggled = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        let Some(row) = event
            .target()
            .and_then(|node| node.dyn_into::<web_sys::HtmlDetailsElement>().ok())
        else {
            return;
        };
        if !row.open() {
            return;
        }
        let Some(url) = row.get_attribute("data-turns") else {
            return;
        };
        let Ok(Some(slot)) = row.query_selector("[data-turns-slot]") else {
            return;
        };
        let Ok(request) = web_sys::XmlHttpRequest::new() else {
            return;
        };
        if request.open("GET", &url).is_err() {
            return;
        }
        request.set_timeout(15_000);
        // Loading: a second toggle must not ask again.
        let _ = row.remove_attribute("data-turns");
        let (response, retry, again) = (request.clone(), row.clone(), url.clone());
        let finished = Closure::once_into_js(move |_: web_sys::Event| {
            match (response.status().ok() == Some(200))
                .then(|| response.response_text().ok().flatten())
                .flatten()
            {
                Some(text) => slot.set_outer_html(&text),
                None => {
                    let _ = retry.set_attribute("data-turns", &again);
                }
            }
        });
        request.set_onloadend(Some(finished.unchecked_ref()));
        if request.send().is_err() {
            let _ = row.set_attribute("data-turns", &url);
        }
    });
    // `toggle` does not bubble; listen while it is captured.
    document.add_event_listener_with_callback_and_bool("toggle", toggled.as_ref().unchecked_ref(), true)?;
    toggled.forget();
    Ok(true)
}

/// Forms marked `data-confirm` (destructive actions such as stopping a service
/// or deleting a workspace) submit only after the browser's confirmation.
/// Without the client they submit directly, as before.
fn attach_confirmations(document: &web_sys::Document) -> Result<(), JsValue> {
    let submit = Closure::<dyn FnMut(web_sys::SubmitEvent)>::new(move |event: web_sys::SubmitEvent| {
        let Some(form) = event.target().map(|target| target.unchecked_into::<web_sys::Element>()) else {
            return;
        };
        let Some(message) = form.get_attribute("data-confirm") else {
            return;
        };
        let confirmed = web_sys::window()
            .and_then(|window| window.confirm_with_message(&message).ok())
            .unwrap_or(true);
        if !confirmed {
            event.prevent_default();
            event.stop_immediate_propagation();
        }
    });
    document.add_event_listener_with_callback_and_bool("submit", submit.as_ref().unchecked_ref(), true)?;
    submit.forget();
    Ok(())
}
fn main() -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| JsValue::from_str("No document"))?;
    attach_confirmations(&document)?;
    if attach_live(&document)? {
        return Ok(());
    }
    let disclosed = attach_disclosures(&document)?;
    let paged = attach_more(&document)?;
    if canvas::attach(&document)? || paged || disclosed {
        return Ok(());
    }
    if let Some(input) = document.query_selector(".conversation-composer textarea[name=message]")? {
        attach_composer(&document, input.dyn_into()?)?;
        attach_transcript(&document)?;
        attach_navigation(&document)?;
        attach_updates(&document)?;
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
#[path = "main/tests.rs"]
mod tests;
