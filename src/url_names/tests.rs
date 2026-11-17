use super::*;

#[test]
fn legacy_paths_map_to_current_names_with_their_tail() {
    for (old, new) in [
        ("/chats", "/dms"),
        ("/chats/anything/else", "/dms/anything/else"),
        ("/chat", "/dms"),
        ("/chat/c1", "/dm/c1"),
        ("/chat/c1/transcript", "/dm/c1/transcript"),
        ("/chat/c1/requests", "/dm/c1/requests"),
        ("/chat/c1/project", "/dm/c1/project"),
        ("/chat/c1/compaction/read", "/dm/c1/compaction/read"),
        ("/chat/c1/context", "/dm/c1/context"),
        ("/chat/c1/messages", "/dm/c1/messages"),
        ("/lenses", "/dna"),
        ("/lenses/save", "/dna/save"),
        ("/lenses/export", "/dna/export"),
    ] {
        assert_eq!(renamed(old).as_deref(), Some(new), "{old}");
    }
}

#[test]
fn current_and_unrelated_paths_are_not_renamed() {
    for path in ["/", "/dms", "/dm/c1", "/dna", "/dna/save", "/chatsx", "/chatter", "/lensesx", "/knowledge", "/v1/chat/completions", "/chats/history"] {
        assert_eq!(renamed(path), None, "{path}");
    }
}

#[test]
fn redirects_keep_the_query_and_preserve_non_get_methods() {
    assert_eq!(
        redirect("GET", "/chat/c1/context", "run=r1&attempt=2"),
        Some(("301 Moved Permanently", "/dm/c1/context?run=r1&attempt=2".to_owned()))
    );
    assert_eq!(redirect("HEAD", "/lenses", ""), Some(("301 Moved Permanently", "/dna".to_owned())));
    assert_eq!(redirect("POST", "/chat/c1/messages", ""), Some(("308 Permanent Redirect", "/dm/c1/messages".to_owned())));
    assert_eq!(redirect("POST", "/lenses/save", "x=1"), Some(("308 Permanent Redirect", "/dna/save?x=1".to_owned())));
    assert_eq!(redirect("GET", "/dm/c1", "a=b"), None);
}

#[test]
fn dm_routes_accept_stored_legacy_paths() {
    assert_eq!(dm_route("/dm/c1/project"), Some("c1/project"));
    assert_eq!(dm_route("/chat/c1"), Some("c1"));
    assert_eq!(dm_route("/dms"), None);
    assert_eq!(dm("c1"), "/dm/c1");
}
