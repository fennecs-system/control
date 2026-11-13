
use super::*;

#[test]
fn page_assembly_preserves_literal_template_and_demo_text_in_documents() {
    let text = "{{tools}} DEMO AGENTS 3 sources · 2 tools {{title}} {{lens}} {{content}}";
    let mut state = State {
        note: text.into(),
        ..Default::default()
    };
    knowledge::migrate(&mut state);
    let page = render(&state, "editor", "engineering", "general");
    assert!(
        page.contains(text),
        "Document text was rewritten by page assembly"
    );
    assert_eq!(state.documents[0].body, text);
}

#[test]
fn socket_writes_release_state_even_when_the_client_stalls_or_disconnects() {
    struct DisconnectedClient<'a>(&'a Mutex<State>);
    impl Write for DisconnectedClient<'_> {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            let state = self.0.try_lock().expect("socket write held shared state");
            assert_eq!(state.runtime.notice, "committed");
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let state = Mutex::new(State::default());
    state_response(
        &mut DisconnectedClient(&state),
        &state,
        |response, state| {
            state.runtime.notice = "committed".into();
            session_http::handle(response, state, "GET", "/chats", "none", "", b"");
        },
    );
    assert_eq!(state.lock().unwrap().runtime.notice, "committed");
}

#[test]
fn legacy_session_view_redirects_to_the_canonical_chat_page() {
    let mut state = State::default();
    let id = chat::create(&mut state);
    let location = |query: &str| legacy_session_location(&state, &fields(query));
    assert_eq!(
        location(&format!("view=session&channel={id}")),
        Some(format!("/chat/{id}"))
    );
    assert_eq!(
        location("view=session&channel=c-missing").as_deref(),
        Some("/chats")
    );
    assert_eq!(
        location("view=session&channel=..%2F..%2Ftools%0D%0ASet-Cookie%3A%20x").as_deref(),
        Some("/chats")
    );
    assert_eq!(location("view=session").as_deref(), Some("/chats"));
    assert_eq!(location(&format!("view=runs&channel={id}")), None);
    assert_eq!(location(""), None);
}
#[test]
fn public_root_never_reflects_query_html_and_bodies_need_a_valid_length() {
    let state = State::default();
    for view in INTERNAL_VIEWS {
        let query = fields(&format!(
            "view={view}&channel=%3Cscript%3Ealert(1)%3C%2Fscript%3E"
        ));
        assert!(
            !public_render(&state, &query, "none").contains("<script>alert(1)"),
            "{view}"
        );
    }
    assert!(matches!(
        body_length("POST / HTTP/1.1\r\nContent-Length: 12\r\n\r\n"),
        Body::Length(12)
    ));
    assert!(matches!(
        body_length("GET / HTTP/1.1\r\n\r\n"),
        Body::Length(0)
    ));
    assert!(matches!(
        body_length("POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
        Body::Chunked
    ));
    for bad in [
        "Content-Length: x",
        "Content-Length: -1",
        "Content-Length: 1\r\nContent-Length: 2",
    ] {
        assert!(
            matches!(
                body_length(&format!("POST / HTTP/1.1\r\n{bad}\r\n\r\n")),
                Body::Invalid
            ),
            "{bad}"
        );
    }
}
#[test]
fn poisoned_state_returns_503_without_rendering_or_mutating() {
    let state = Mutex::new(State::default());
    let _ = std::panic::catch_unwind(|| {
        let _guard = state.lock().unwrap();
        panic!("simulate interrupted state mutation");
    });
    let mut response = Vec::new();
    state_response(&mut response, &state, |_, _| panic!("poisoned state used"));
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));
    let (header, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(header.contains(&format!("Content-Length: {}\r\n", body.len())));
    assert!(state.is_poisoned());
    // Static UI dependencies remain available without touching mutable state.
    let mut stylesheet = Vec::new();
    assert!(serve_stylesheet(&mut stylesheet, "/style.css"));
    assert!(stylesheet.starts_with(b"HTTP/1.1 200 OK\r\n"));
}
#[test]
fn encoded_maximum_chat_messages_fit_their_transport_limits() {
    let text = "🦊".repeat(control_context::limits::CHAT_MESSAGE_CHARS);
    let form = format!(
        "message={}",
        "%F0%9F%A6%8A".repeat(control_context::limits::CHAT_MESSAGE_CHARS)
    );
    assert_eq!(fields(&form)["message"], text);
    for target in ["/chats", "/chats?new=1", "/chat/c-test/messages"] {
        assert!(form.len() < body_limit(target));
    }
    let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control.chat.send","arguments":{"id":"c-test","message":text}}}).to_string();
    assert!(request.len() < body_limit("/mcp"));
    assert!(request.len() + 1 < control_context::limits::CHAT_REQUEST_BYTES);
    assert_eq!(body_limit("/rooms/general"), 32768);
    assert_eq!(body_limit("/knowledge/save"), 512 * 1024);
    assert_eq!(body_limit("/save"), 512 * 1024);
    assert_eq!(body_limit("/lenses/save"), 512 * 1024);
    assert_eq!(body_limit("/chat/invalid/path/messages"), 32768);
}
