use std::{cell::{Cell, RefCell}, rc::Rc};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};

use control_context::commands::{self, DEFINITIONS as COMMANDS};

fn completions(value: &str) -> Vec<usize> { commands::completions(value) }
fn command_length(value: &str) -> usize {
    commands::parse(value).filter(|c|c.highlight.start==0).map_or(0,|c|c.highlight.end)
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
        self.input.set_custom_validity(if control_context::limits::message_too_long(&value) {
            control_context::limits::CHAT_MESSAGE_ERROR
        } else { "" });
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
    let options = COMMANDS.iter()
        .map(|command| format!("command-{}", command.id))
        .map(|id| document.get_element_by_id(&id).map(Ok).unwrap_or_else(|| document.create_element("button")))
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
                            || !COMMANDS.iter().any(|command|command.text == value.trim_end()) =>
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
                if !event.repeat() && !value.trim().is_empty()
                    && let Some(form) = target.input.form() {
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
    let Some(form)=composer.input.form().filter(|form|form.id()=="session-message") else{return Ok(());};
    let error=composer.document.create_element("p")?;
    error.set_attribute("role","alert")?;
    error.set_attribute("hidden","")?;
    form.append_child(&error)?;
    let button=form.query_selector(".send-message")?;
    let busy=Rc::new(Cell::new(false));
    let target=composer.clone();let submitted_form=form.clone();
    let submit=Closure::<dyn FnMut(web_sys::Event)>::new(move |event:web_sys::Event| {
        if target.input.value().trim()=="/open" {return;}
        if busy.get(){event.prevent_default();return;}
        // Preserve native form fallback if browser APIs cannot initialize.
        let prepare=||->Result<_,JsValue>{
            let data=web_sys::FormData::new_with_form(&submitted_form)?;
            let params=web_sys::UrlSearchParams::new()?;
            for entry in js_sys::try_iter(&data)?.ok_or_else(||JsValue::from_str("No form iterator"))? {
                let pair=js_sys::Array::from(&entry?);
                if let (Some(key),Some(value))=(pair.get(0).as_string(),pair.get(1).as_string()) {params.append(&key,&value);}
            }
            let request=web_sys::XmlHttpRequest::new()?;
            request.open("POST",&submitted_form.action())?;
            request.set_request_header("Accept","application/json")?;
            request.set_request_header("Content-Type","application/x-www-form-urlencoded")?;
            request.set_timeout(15000);
            Ok((request,String::from(params.to_string())))
        };
        let Ok((request,payload))=prepare() else{return;};
        event.prevent_default();busy.set(true);
        let _=error.set_attribute("hidden","");
        if let Some(button)=&button {let _=button.set_attribute("disabled","");}
        let response=request.clone();let done=busy.clone();let target=target.clone();let error=error.clone();let completed_button=button.clone();
        let send_error=error.clone();
        let submitted=target.input.value();
        let finished=Closure::<dyn FnMut(web_sys::Event)>::once(move |_| {
            done.set(false);
            if let Some(button)=&completed_button {let _=button.remove_attribute("disabled");}
            let value=response.response_text().ok().flatten().and_then(|text|serde_json::from_str::<serde_json::Value>(&text).ok());
            if response.status().ok()==Some(200) && value.as_ref().is_some_and(|v|v["title"].is_string()) {
                let value=value.unwrap();
                if target.input.value()==submitted {target.input.set_value("");}
                target.selected.set(None);target.dismissed.set(true);let _=target.refresh();
                if let Ok(Some(title))=target.document.query_selector(".conversation-heading h2") {title.set_text_content(value["title"].as_str());}
                if let Ok(Some(heading))=target.document.query_selector(".conversation-heading") {
                    if let Ok(Some(mode))=heading.query_selector(".active-command") {mode.remove();}
                    if value["improve"]==true && let Ok(mode)=target.document.create_element("span") {
                        let _=mode.set_attribute("class","chat-command active-command");
                        mode.set_text_content(Some("/improve"));let _=heading.append_child(&mode);
                    }
                }
                if let Ok(Some(frame))=target.document.query_selector("iframe.chat-transcript")
                    && let Ok(frame)=frame.dyn_into::<web_sys::HtmlIFrameElement>()
                    && let Some(doc)=frame.content_document()
                    && let Some(scroller)=doc.scrolling_element() {scroller.set_scroll_top(scroller.scroll_height());}
                if let Ok(event)=web_sys::Event::new("control-transcript-sent") {let _=target.document.dispatch_event(&event);}
                let _=target.input.focus();
            } else {
                let message=value.as_ref().and_then(|value|value["error"].as_str()).unwrap_or("Could not confirm delivery. Check the conversation before sending again.");
                error.set_text_content(Some(message));let _=error.remove_attribute("hidden");
            }
        });
        request.set_onloadend(Some(finished.as_ref().unchecked_ref()));
        if request.send_with_opt_str(Some(&payload)).is_err() {
            busy.set(false);if let Some(button)=&button {let _=button.remove_attribute("disabled");}
            send_error.set_text_content(Some("Could not confirm delivery. Check the conversation before sending again."));
            let _=send_error.remove_attribute("hidden");
        }
        finished.forget();
    });
    form.add_event_listener_with_callback("submit",submit.as_ref().unchecked_ref())?;
    submit.forget();Ok(())
}

// Stable root identities retain old messages and interactive forms. Only changed
// streaming blocks are replaced; an append never rebuilds the preceding history.
fn keyed_nodes(body: &web_sys::HtmlElement) -> Vec<(String,web_sys::Node)> {
    let mut counts=std::collections::HashMap::<String,usize>::new();
    let nodes=body.child_nodes();
    (0..nodes.length()).filter_map(|index|nodes.item(index)).map(|node|{
        // `instanceof Element` fails across iframe realms in Chromium. The DOM
        // node type is realm-independent; this cast follows that explicit check.
        let base=if node.node_type()==web_sys::Node::ELEMENT_NODE {
            let element=node.unchecked_ref::<web_sys::Element>();
            if let Some(key)=element.get_attribute("data-transcript-key") {format!("event:{key}")}
            else if !element.id().is_empty(){format!("id:{}",element.id())}
            else{format!("{}:{}",element.tag_name(),element.get_attribute("aria-label").unwrap_or_default())}
        }else{format!("node:{}",node.node_type())};
        let count=counts.entry(base.clone()).or_default();let key=format!("{base}:{count}");*count+=1;(key,node)
    }).collect()
}
fn reconcile(body:&web_sys::HtmlElement,new_body:&web_sys::HtmlElement)->Result<(),JsValue> {
    let old=keyed_nodes(body);let wanted=keyed_nodes(new_body);
    let keys:std::collections::HashSet<_>=wanted.iter().map(|(key,_)|key.as_str()).collect();
    for (key,node) in &old {if !keys.contains(key.as_str()){body.remove_child(node)?;}}
    let old:std::collections::HashMap<_,_>=old.into_iter().collect();
    let mut cursor=body.first_child();
    for (key,incoming) in wanted {
        let node=if let Some(existing)=old.get(&key) {
            // An older poll must not reset an in-flight decision or resurrect
            // its buttons after the server has acknowledged it. Once the port
            // leaves the next snapshot, the normal removal above still applies.
            let local_decision=existing.node_type()==web_sys::Node::ELEMENT_NODE && {
                let element=existing.unchecked_ref::<web_sys::Element>();
                element.matches(".port-request")? && (element.has_attribute("data-port-decided")
                    || element.query_selector("form[data-submitting]")?.is_some())
            };
            if local_decision || existing.is_equal_node(Some(&incoming)) {existing.clone()}
            else {body.replace_child(&incoming,existing)?;if cursor.as_ref().is_some_and(|n|n.is_same_node(Some(existing))){cursor=Some(incoming.clone());}incoming}
        }else{incoming};
        if cursor.as_ref().is_some_and(|n|n.is_same_node(Some(&node))){cursor=node.next_sibling();}
        else{body.insert_before(&node,cursor.as_ref())?;}
    }
    Ok(())
}

// The parent owns updates: browser-managed iframe refreshes can stall in Chromium
// and visibly reload in Firefox. Neither the frame nor the composer is navigated.
fn follow_transcript(frame: &web_sys::HtmlIFrameElement, following: Rc<Cell<bool>>) -> Result<(),JsValue> {
    let Some(doc) = frame.content_document() else { return Ok(()); };
    let Some(scroller) = doc.scrolling_element() else { return Ok(()); };
    let target = scroller.clone();
    let state = following.clone();
    let scroll = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        state.set(target.scroll_height()-target.client_height()-target.scroll_top()<80);
    });
    doc.add_event_listener_with_callback("scroll",scroll.as_ref().unchecked_ref())?;
    scroll.forget();
    let resize = Closure::<dyn FnMut()>::new(move || {
        if following.get() { scroller.set_scroll_top(scroller.scroll_height()); }
    });
    let observer = web_sys::ResizeObserver::new(resize.as_ref().unchecked_ref())?;
    if let Some(body) = doc.body() { observer.observe(&body); }
    // Keep the observer alive for this document's lifetime; replacing the frame
    // document on navigation releases its observed nodes with the page.
    std::mem::forget(observer);
    resize.forget();
    Ok(())
}
fn attach_port_approvals(frame:&web_sys::HtmlIFrameElement)->Result<(),JsValue> {
    let Some(document)=frame.content_document() else{return Ok(());};
    let Some(root)=document.document_element() else{return Ok(());};
    if root.has_attribute("data-port-handler"){return Ok(());}
    root.set_attribute("data-port-handler","")?;
    let submit=Closure::<dyn FnMut(web_sys::SubmitEvent)>::new(move |event:web_sys::SubmitEvent| {
        // Submit targets are forms. Use a realm-independent tag check rather
        // than instanceof, which rejects iframe forms in Chromium.
        let Some(form)=event.target().map(|target|target.unchecked_into::<web_sys::HtmlFormElement>()) else{return;};
        let Some(action)=form.get_attribute("action").filter(|action|matches!(action.as_str(),"/tools/ports"|"/tools/bots")) else{return;};
        if form.tag_name()!="FORM"{return;}
        if form.has_attribute("data-submitting"){event.prevent_default();return;}
        let prepare=||->Result<_,JsValue> {
            let params=web_sys::UrlSearchParams::new()?;
            let data=web_sys::FormData::new_with_form(&form)?;
            for entry in js_sys::try_iter(&data)?.ok_or_else(||JsValue::from_str("No form iterator"))? {
                let pair=js_sys::Array::from(&entry?);
                if let (Some(key),Some(value))=(pair.get(0).as_string(),pair.get(1).as_string()){params.append(&key,&value);}
            }
            if let Some(button)=event.submitter() && let Some(name)=button.get_attribute("name") {
                params.append(&name,&button.get_attribute("value").unwrap_or_default());
            }
            params.append("response","json");
            let request=web_sys::XmlHttpRequest::new()?;
            request.open("POST",&action)?;
            request.set_request_header("Content-Type","application/x-www-form-urlencoded")?;
            request.set_timeout(15000);
            Ok((request,String::from(params.to_string())))
        };
        let Ok((request,payload))=prepare() else{return;};
        event.prevent_default();let _=form.set_attribute("data-submitting","");
        let _=form.set_attribute("aria-busy","true");
        if let Ok(Some(error))=form.query_selector("[role=alert]"){let _=error.set_attribute("hidden","");}
        let controls=form.query_selector_all("button, input, select").ok().map(|nodes| {
            (0..nodes.length()).filter_map(|index|nodes.item(index)).map(|node|node.unchecked_into::<web_sys::Element>())
                .filter(|element|!element.has_attribute("disabled")).collect::<Vec<_>>()
        }).unwrap_or_default();
        for control in &controls {let _=control.set_attribute("disabled","");}
        let button=event.submitter().map(|button| {
            let label=button.text_content().unwrap_or_default();
            button.set_text_content(Some(if matches!(button.get_attribute("name").as_deref(),Some("revoke"|"deny")){"Denying…"}else{"Allowing…"}));
            (button,label)
        });
        let response=request.clone();let failed_form=form.clone();let failed_controls=controls.clone();let failed_button=button.clone();
        let finished=Closure::<dyn FnMut(web_sys::Event)>::once(move |_| {
            let value=response.response_text().ok().flatten().and_then(|text|serde_json::from_str::<serde_json::Value>(&text).ok());
            if response.status().ok()==Some(200) && value.as_ref().is_some_and(|value|value["ok"]==true) {
                if let Ok(Some(prompt))=form.closest(".port-request") {
                    let _=prompt.set_attribute("data-port-decided","");
                    let _=prompt.set_attribute("hidden","");
                }
                if let Some(parent)=web_sys::window().and_then(|window|window.document()) && let Ok(event)=web_sys::Event::new("control-transcript-refresh"){let _=parent.dispatch_event(&event);}
            }else {
                restore_port_form(&form,&controls,button.as_ref());
                if let Ok(Some(error))=form.query_selector("[role=alert]") {
                    error.set_text_content(Some(value.as_ref().and_then(|value|value["error"].as_str()).unwrap_or("Could not confirm the decision. Check status before retrying.")));
                    let _=error.remove_attribute("hidden");
                }
            }
        });
        request.set_onloadend(Some(finished.as_ref().unchecked_ref()));
        if request.send_with_opt_str(Some(&payload)).is_err(){
            restore_port_form(&failed_form,&failed_controls,failed_button.as_ref());
            if let Ok(Some(error))=failed_form.query_selector("[role=alert]") {
                error.set_text_content(Some("Could not confirm the decision. Check status before retrying."));
                let _=error.remove_attribute("hidden");
            }
        }
        finished.forget();
    });
    document.add_event_listener_with_callback("submit",submit.as_ref().unchecked_ref())?;
    submit.forget();Ok(())
}
fn restore_port_form(form:&web_sys::HtmlFormElement,controls:&[web_sys::Element],button:Option<&(web_sys::HtmlElement,String)>) {
    let _=form.remove_attribute("data-submitting");let _=form.remove_attribute("aria-busy");
    for control in controls {let _=control.remove_attribute("disabled");}
    if let Some((button,label))=button {button.set_text_content(Some(label));}
}
fn attach_transcript(document: &web_sys::Document) -> Result<(), JsValue> {
    let frames = document.query_selector_all("iframe[data-live-chat]")?;
    for index in 0..frames.length() {
        if let Some(frame) = frames.item(index) { attach_chat_frame(document,frame.unchecked_into())?; }
    }
    Ok(())
}
fn update_request_count(document:&web_sys::Document, body:&web_sys::HtmlElement) {
    let Some(count) = body.get_attribute("data-request-count") else { return; };
    let Ok(Some(toggle)) = document.query_selector(".chat-requests") else { return; };
    let previous = toggle.get_attribute("data-count").and_then(|v|v.parse::<usize>().ok()).unwrap_or(0);
    let current = count.parse::<usize>().unwrap_or(0);
    if let Ok(Some(label)) = toggle.query_selector(".request-count") { label.set_text_content(Some(&count)); }
    if current > previous { let _=toggle.set_attribute("open",""); }
    let _=toggle.set_attribute("data-count",&count);
}
fn attach_chat_frame(document: &web_sys::Document, frame:web_sys::HtmlIFrameElement) -> Result<(), JsValue> {
    let transcript = frame.matches(".chat-transcript")?;
    let following = Rc::new(Cell::new(transcript));
    let loaded_frame = frame.clone();
    // A sent message always returns the reader to the end, like a chat client.
    let sent_following = following.clone();
    let sent_document = document.clone();
    let sent = Closure::<dyn FnMut()>::new(move || {
        sent_following.set(transcript);
        if let Ok(event) = web_sys::Event::new("control-transcript-refresh") { let _ = sent_document.dispatch_event(&event); }
    });
    document.add_event_listener_with_callback("control-transcript-sent", sent.as_ref().unchecked_ref())?;
    sent.forget();
    let loaded_following = following.clone();
    let parent_document=document.clone();
    let loaded = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        loaded_following.set(transcript);
        if transcript { let _ = follow_transcript(&loaded_frame,loaded_following.clone()); }
        let _ = attach_port_approvals(&loaded_frame);
        if let Some(body)=loaded_frame.content_document().and_then(|d|d.body()) { update_request_count(&parent_document,&body); }
    });
    frame.add_event_listener_with_callback("load",loaded.as_ref().unchecked_ref())?;
    loaded.forget();
    if transcript { follow_transcript(&frame,following.clone())?; }
    attach_port_approvals(&frame)?;
    if let Some(body)=frame.content_document().and_then(|d|d.body()) { update_request_count(document,&body); }
    let url = frame.get_attribute("src").unwrap_or_default().split('#').next().unwrap_or_default().to_owned();
    let busy = Rc::new(Cell::new(false));
    let pending = Rc::new(Cell::new(false));
    let previous = Rc::new(RefCell::new(String::new()));
    let parent_document=document.clone();
    let poll = Closure::<dyn FnMut()>::new(move || {
        if busy.replace(true) { pending.set(true); return; }
        let Ok(request) = web_sys::XmlHttpRequest::new() else { busy.set(false); return; };
        if request.open("GET", &url).is_err() { busy.set(false); return; }
        request.set_timeout(10_000);
        let response = request.clone();
        let frame = frame.clone();
        let following = following.clone();
        let previous = previous.clone();
        let completed = busy.clone();
        let pending = pending.clone();
        let parent_document=parent_document.clone();
        let finished = Closure::<dyn FnMut(web_sys::Event)>::once(move |_| {
            completed.set(false);
            if pending.replace(false) && let Ok(event) = web_sys::Event::new("control-transcript-refresh") {
                let _ = parent_document.dispatch_event(&event);
            }
            if response.status().ok() != Some(200) {
                let retry = Closure::<dyn FnMut()>::once(move || {
                    if let Ok(event) = web_sys::Event::new("control-transcript-refresh") { let _ = parent_document.dispatch_event(&event); }
                });
                let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(retry.as_ref().unchecked_ref(), 1000);
                retry.forget();
                return;
            }
            let Some(text) = response.response_text().ok().flatten() else { return; };
            if *previous.borrow() == text { return; }
            let Some(doc) = frame.content_document() else { return; };
            let Some(body) = doc.body() else { return; };
            let Ok(parser) = web_sys::DomParser::new() else { return; };
            let Ok(parsed) = parser.parse_from_string(&text, web_sys::SupportedType::TextHtml) else { return; };
            let Some(new_body) = parsed.body() else { return; };
            update_request_count(&parent_document,&new_body);
            let Some(scroller) = doc.scrolling_element() else { return; };
            let top = scroller.scroll_top();
            let at_end = following.get();
            let mut details = std::collections::HashMap::new();
            if let Ok(nodes) = body.query_selector_all("details") {
                for index in 0..nodes.length() {
                    if let Some(node) = nodes.item(index).map(|n| n.unchecked_into::<web_sys::HtmlDetailsElement>())
                        && let Some(key) = node.get_attribute("data-transcript-key") { details.insert(key, node.open()); }
                }
            }
            if let Ok(nodes) = new_body.query_selector_all("details") {
                for index in 0..nodes.length() {
                    if let Some(node) = nodes.item(index).map(|n| n.unchecked_into::<web_sys::HtmlDetailsElement>())
                        && let Some(open) = node.get_attribute("data-transcript-key").and_then(|key|details.get(&key)) { node.set_open(*open); }
                }
            }
            if reconcile(&body, &new_body).is_err() { return; }
            scroller.set_scroll_top(if at_end { scroller.scroll_height() } else { top });
            // Layout can settle after reconciliation (fonts, details and frame
            // sizing). Follow again at the next paint, but respect scrollback.
            let callback = Closure::<dyn FnMut(f64)>::once(move |_| {
                if following.get()==at_end {
                    scroller.set_scroll_top(if at_end {scroller.scroll_height()} else {top});
                }
            });
            let _ = web_sys::window().unwrap().request_animation_frame(callback.as_ref().unchecked_ref());
            callback.forget();
            *previous.borrow_mut() = text;
        });
        request.set_onloadend(Some(finished.as_ref().unchecked_ref()));
        if request.send().is_err() { busy.set(false); }
        finished.forget();
    });
    document.add_event_listener_with_callback("control-transcript-refresh",poll.as_ref().unchecked_ref())?;
    poll.forget();
    Ok(())
}

// Refresh only the disclosed picker; keep its scroll and expanded branches.
fn attach_navigation(document:&web_sys::Document)->Result<(),JsValue> {
    let Some(toggle)=document.query_selector(".conversation-toggle")? else{return Ok(());};
    // Presentation state only. The server renders the panel from this cookie,
    // so the first paint is already correct. Remember clicks before a link
    // changes pages.
    if let Ok(html)=document.clone().dyn_into::<web_sys::HtmlDocument>() {
        let target=toggle.clone();
        let remember=Closure::<dyn FnMut(web_sys::Event)>::new(move |event:web_sys::Event| {
            if event.type_()=="click" && event.target().and_then(|node|node.dyn_into::<web_sys::Element>().ok())
                .and_then(|element|element.closest("a[href]").ok().flatten()).is_none() {return;}
            let state=if target.has_attribute("open") {"open"}else{"closed"};
            let _=html.set_cookie(&format!("control_conversations={state}; Path=/; Max-Age=31536000; SameSite=Strict"));
        });
        for event in ["toggle","click"] {
            toggle.add_event_listener_with_callback(event,remember.as_ref().unchecked_ref())?;
        }
        remember.forget();
    }
    let Some(frame)=document.query_selector("iframe.chat-transcript")? else{return Ok(());};
    let source=frame.get_attribute("src").unwrap_or_default();
    let Some(chat)=source.split('/').nth(2) else{return Ok(());};
    let url=format!("/chat/{chat}/conversations");
    let busy=Rc::new(Cell::new(false));let previous=Rc::new(RefCell::new(String::new()));
    let pending=Rc::new(Cell::new(false));let parent=document.clone();
    let target=toggle.clone();
    let poll=Rc::new(Closure::<dyn FnMut()>::new(move || {
        if !target.has_attribute("open") {return;}
        if busy.replace(true) {pending.set(true);return;}
        let Ok(request)=web_sys::XmlHttpRequest::new() else{busy.set(false);return;};
        if request.open("GET",&url).is_err(){busy.set(false);return;}
        request.set_timeout(10000);
        let response=request.clone();let target=target.clone();let completed=busy.clone();let previous=previous.clone();
        let pending=pending.clone();let parent=parent.clone();
        let done=Closure::<dyn FnMut(web_sys::Event)>::once(move |_| {
            completed.set(false);
            if pending.replace(false) && let Ok(event)=web_sys::Event::new("control-transcript-refresh"){let _=parent.dispatch_event(&event);}
            if response.status().ok()!=Some(200){return;}
            let Some(text)=response.response_text().ok().flatten() else{return;};
            if *previous.borrow()==text{return;}
            let Ok(Some(aside))=target.query_selector(".conversation-navigation") else{return;};
            let old_scroll=aside.query_selector("nav").ok().flatten().map_or(0,|nav|nav.scroll_top());
            let mut expanded=std::collections::HashMap::new();
            if let Ok(nodes)=aside.query_selector_all("details[data-chat-branch]") {for index in 0..nodes.length(){if let Some(node)=nodes.item(index).and_then(|n|n.dyn_into::<web_sys::HtmlDetailsElement>().ok()) && let Some(id)=node.get_attribute("data-chat-branch"){expanded.insert(id,node.open());}}}
            let Ok(parser)=web_sys::DomParser::new() else{return;};
            let Ok(parsed)=parser.parse_from_string(&text,web_sys::SupportedType::TextHtml) else{return;};
            let Ok(Some(new_aside))=parsed.query_selector(".conversation-navigation") else{return;};
            if let Ok(nodes)=new_aside.query_selector_all("details[data-chat-branch]"){for index in 0..nodes.length(){if let Some(node)=nodes.item(index).and_then(|n|n.dyn_into::<web_sys::HtmlDetailsElement>().ok()) && let Some(open)=node.get_attribute("data-chat-branch").and_then(|id|expanded.get(&id)){node.set_open(*open);}}}
            // The first poll can equal server-rendered content. Keep live links
            // and focus instead of replacing an unchanged tree during a click.
            if !aside.is_equal_node(Some(new_aside.unchecked_ref())) {
                aside.set_inner_html(&new_aside.inner_html());
                if let Ok(Some(nav))=aside.query_selector("nav"){nav.set_scroll_top(old_scroll);}
            }
            *previous.borrow_mut()=text;
        });
        request.set_onloadend(Some(done.as_ref().unchecked_ref()));
        if request.send().is_err(){busy.set(false);}
        done.forget();
    }));
    // State-change hints and disclosure use the same Rust callback.
    toggle.add_event_listener_with_callback("toggle",poll.as_ref().as_ref().unchecked_ref())?;
    document.add_event_listener_with_callback("control-transcript-refresh",poll.as_ref().as_ref().unchecked_ref())?;
    std::mem::forget(poll);
    Ok(())
}

// One authenticated, payload-free stream per page. Reconnects refresh the
// authoritative HTML; a slow recovery poll is used only while disconnected.
fn attach_updates(document: &web_sys::Document) -> Result<(), JsValue> {
    let stream = web_sys::EventSource::new("/events")?;
    let target = document.clone();
    let changed = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        if let Ok(event) = web_sys::Event::new("control-transcript-refresh") { let _ = target.dispatch_event(&event); }
    });
    stream.add_event_listener_with_callback("message", changed.as_ref().unchecked_ref())?;
    changed.forget();
    let recovery = stream.clone();
    let target = document.clone();
    let fallback = Closure::<dyn FnMut()>::new(move || {
        if recovery.ready_state() != web_sys::EventSource::OPEN && let Ok(event) = web_sys::Event::new("control-transcript-refresh") {
            let _ = target.dispatch_event(&event);
        }
    });
    let window = web_sys::window().unwrap();
    window.set_interval_with_callback_and_timeout_and_arguments_0(fallback.as_ref().unchecked_ref(), 10_000)?;
    fallback.forget();
    // The page owns the connection. Let the browser close/suspend it with the
    // document so returning through the back/forward cache can reconnect.
    Ok(())
}

/// Pages that render from state first mark a `data-live-src` region; slow
/// engine reads arrive afterwards as HTML fragments keyed by `data-live`.
fn attach_live(document: &web_sys::Document) -> Result<bool, JsValue> {
    let Some(root) = document.query_selector("[data-live-src]")? else { return Ok(false); };
    let Some(url) = root.get_attribute("data-live-src") else { return Ok(false); };
    let request = web_sys::XmlHttpRequest::new()?;
    request.open("GET", &url)?;
    request.set_timeout(15_000);
    let response = request.clone();
    let finished = Closure::<dyn FnMut(web_sys::Event)>::once(move |_| {
        let fragments = (response.status().ok() == Some(200))
            .then(|| response.response_text().ok().flatten())
            .flatten()
            .and_then(|text| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text).ok())
            .unwrap_or_default();
        let Ok(slots) = root.query_selector_all("[data-live]") else { return; };
        for index in 0..slots.length() {
            let Some(slot) = slots.item(index).map(|node| node.unchecked_into::<web_sys::Element>()) else { continue; };
            match slot.get_attribute("data-live").and_then(|id| fragments.get(&id)?.as_str().map(str::to_owned)) {
                Some(html) => slot.set_inner_html(&html),
                None => slot.set_inner_html("<p class='muted'>Usage unavailable.</p>"),
            }
        }
    });
    request.set_onloadend(Some(finished.as_ref().unchecked_ref()));
    request.send()?;
    finished.forget();
    Ok(true)
}

fn main() -> Result<(), JsValue> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| JsValue::from_str("No document"))?;
    if attach_live(&document)? {
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
