
use super::*;
#[test]
fn search_parses_both_providers_and_rejects_active_urls() {
    let ddg = "<div class='result'><a class='result__a' href='//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F'>Rust &amp; tools</a><p class='result__snippet'>A language.</p></div><div class='result'><a class='result__a' href='javascript:bad()'>Bad</a></div>";
    let result = parse_search(ddg, false, 5).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0]["title"], "Rust & tools");
    assert_eq!(result[0]["url"], "https://www.rust-lang.org/");
    let bing = "<li class='b_algo'><h2><a href='https://example.com'>Example</a></h2><div class='b_caption'><p>Result.</p></div></li>";
    assert_eq!(
        parse_search(bing, true, 1).unwrap()[0]["snippet"],
        "Result."
    );
    assert!(parse_search("<form id='captcha'>Challenge</form>", false, 5).is_err());
    assert!(link("https://user:password@example.com", true).is_none());
    assert!(call("control.rust.run", &json!({"code":"", "mount":"/"})).is_err());
    assert!(
        call(
            "control.web.search",
            &json!({"query":"", "url":"http://localhost"})
        )
        .is_err()
    );
}
#[test]
fn playground_output_is_drained_past_the_cap_and_marked_truncated() {
    let input = vec![b'x'; 200_000];
    let (kept, truncated) = drain(&input[..], 65536).unwrap();
    assert_eq!((kept.len(), truncated), (65536, true));
    assert_eq!(
        drain(&b"run 0\nok"[..], 65536).unwrap(),
        (b"run 0\nok".to_vec(), false)
    );
}
#[test]
#[ignore = "requires the pinned Nix development image; no model calls"]
fn live_rust_playground_success_errors_and_timeout() {
    let ok = playground("fn main() { println!(\"control-rust-ok\"); }").unwrap();
    assert_eq!(ok["success"], true);
    assert!(ok["output"].as_str().unwrap().contains("control-rust-ok"));
    let err = playground("fn main() { invalid rust }").unwrap();
    assert_eq!(err["success"], false);
    assert!(err["status"].as_str().unwrap().starts_with("compile "));
    let timeout = playground("fn main() { loop { std::hint::spin_loop(); } }").unwrap();
    assert_eq!(timeout["status"], "run timeout");
}
