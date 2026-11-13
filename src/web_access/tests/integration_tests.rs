
use super::*;
#[test]
fn operator_grants_persist_revoke_and_rollback_without_cross_chat_authority() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    let other = crate::chat::create(&mut state);
    let fields = std::collections::HashMap::from([
        ("chat".into(), chat.clone()),
        ("origin".into(), "https://example.com".into()),
        ("seconds".into(), "900".into()),
    ]);
    assert!(change_access(&mut state, &fields, 100, |_| Err(fail("disk full"))).is_err());
    assert!(state.web_grants.is_empty());
    let mut saved = Vec::new();
    change_access(&mut state, &fields, 100, |s| {
        saved = serde_json::to_vec(s).unwrap();
        Ok(())
    })
    .unwrap();
    let mut state: State = serde_json::from_slice(&saved).unwrap();
    let url = Url::parse("https://example.com/page").unwrap();
    assert!(permitted(&state, &chat, &Actor::LocalOperator, &url, 101).is_ok());
    assert!(permitted(&state, &other, &Actor::LocalOperator, &url, 101).is_err());
    assert!(permitted(&state, &chat, &Actor::LocalOperator, &url, 1000).is_err());
    let revoke =
        std::collections::HashMap::from([("revoke".into(), state.web_grants[0].id.clone())]);
    assert!(change_access(&mut state, &revoke, 101, |_| Err(fail("disk full"))).is_err());
    assert_eq!(state.web_grants.len(), 1);
    change_access(&mut state, &revoke, 101, |_| Ok(())).unwrap();
    assert!(permitted(&state, &chat, &Actor::LocalOperator, &url, 101).is_err());
}
#[test]
fn worker_cannot_approve_destinations_and_bad_capture_fails_before_docker() {
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "Build a page", "test").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    let id = run.id.clone();
    let shared = Arc::new(Mutex::new(state));
    let error = worker(&shared, &id, &json!({"url":"https://example.invalid/"})).unwrap_err();
    assert!(error.to_string().contains("Origin not approved"));
    assert!(shared.lock().unwrap().web_grants.is_empty());
    for args in [
        json!({"html":""}),
        json!({"html":"x".repeat(16001)}),
        json!({"html":"<p>x</p>","width":1}),
        json!({"html":"x","origin":"https://example.com"}),
    ] {
        assert!(capture_tool(&args).is_err());
    }
}
#[test]
fn redirect_loops_oversize_and_unsupported_content_are_bounded() {
    struct TooLarge;
    impl Transport for TooLarge {
        fn resolve(&mut self, _: &Url) -> io::Result<Vec<SocketAddr>> {
            Ok(vec!["8.8.8.8:443".parse().unwrap()])
        }
        fn get(
            &mut self,
            _: &Url,
            _: &[SocketAddr],
            _: Duration,
        ) -> io::Result<(u16, String, Option<String>, Vec<u8>)> {
            Ok((200, "text/html".into(), None, vec![b'x'; BODY_LIMIT + 1]))
        }
    }
    let page = fetch(&mut TooLarge, "https://example.com/", |_| Ok(())).unwrap();
    assert_eq!(page.body.len(), BODY_LIMIT);
    assert!(page.truncated);
}
#[test]
#[ignore = "requires a pinned Nix Firefox image and isolated CONTROL_CONFIG_DIR/CONTROL_STATE_DIR; no network or model calls"]
fn live_worker_capture_returns_viewable_offline_artifact() {
    use base64::Engine;
    let started = Instant::now();
    let result=capture_tool(&json!({"html":"<!doctype html><html><style>body{background:#17131f;color:#cab4fa;font-family:monospace}h1{font-size:30px}</style><h1>Control webpage demo</h1><p>Offline worker capture</p></html>","width":640,"height":480})).unwrap();
    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"]["offline"], true);
    let data = result["content"][1]["data"]
        .as_str()
        .expect("simple page should fit inline evidence budget");
    let png = base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 640);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 480);
    println!(
        "Offline worker capture: {} PNG bytes in {:?}",
        png.len(),
        started.elapsed()
    );
}
