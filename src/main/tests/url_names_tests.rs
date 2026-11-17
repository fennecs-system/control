//! User-facing URL names through the whole request handler: current names
//! serve, legacy names redirect permanently with their tail and query, and
//! rendered pages only link to current names.
use super::*;
use std::io::Read;

const SESSION: &str = "url-names-operator-session";

fn request(state: &Arc<Mutex<State>>, method: &str, target: &str, body: &str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut client = TcpStream::connect(address).unwrap();
    client.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let (server, _) = listener.accept().unwrap();
    let (state, access) = (state.clone(), Arc::new(access::Access::for_tests(SESSION)));
    let handler = std::thread::spawn(move || handle(server, state, access));
    let form = if body.is_empty() { String::new() } else { "Content-Type: application/x-www-form-urlencoded\r\n".to_owned() };
    write!(
        client,
        "{method} {target} HTTP/1.1\r\nHost: {address}\r\nCookie: control_operator={SESSION}\r\n{form}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ).unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    handler.join().unwrap();
    response
}

fn status_and_location(response: &str) -> (&str, Option<&str>) {
    let head = response.split("\r\n\r\n").next().unwrap();
    let status = head.lines().next().unwrap().split_once(' ').unwrap().1;
    let location = head.lines().find_map(|line| line.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("location")).map(|(_, v)| v.trim()));
    (status, location)
}

/// Every `href`, `action`, `src` and refresh `url` in `html` that is a local path.
fn local_links(html: &str) -> Vec<String> {
    let mut links = Vec::new();
    for marker in ["href=", "action=", "src=", "url="] {
        for piece in html.split(marker).skip(1) {
            let piece = piece.trim_start_matches(['\'', '"']);
            let link: String = piece.chars().take_while(|c| !matches!(c, '\'' | '"' | ' ' | '>')).collect();
            if link.starts_with('/') {
                links.push(link);
            }
        }
    }
    links
}

fn fixture() -> (Arc<Mutex<State>>, String) {
    let mut state = State::default();
    let id = chat::create(&mut state);
    (Arc::new(Mutex::new(state)), id)
}

#[test]
fn legacy_urls_redirect_permanently_with_tail_and_query() {
    let (state, id) = fixture();
    for (method, old, new) in [
        ("GET", format!("/chat/{id}"), format!("/dm/{id}")),
        ("GET", format!("/chat/{id}/transcript?after=3"), format!("/dm/{id}/transcript?after=3")),
        ("GET", format!("/chat/{id}/context?run=r-1&attempt=2"), format!("/dm/{id}/context?run=r-1&attempt=2")),
        ("GET", format!("/chat/{id}/compaction/read?node=4"), format!("/dm/{id}/compaction/read?node=4")),
        ("GET", format!("/chat/{id}/compaction"), format!("/dm/{id}/memory")),
        ("GET", format!("/dm/{id}/compaction?block=0.2"), format!("/dm/{id}/memory?block=0.2")),
        ("GET", "/chats?new=1".to_owned(), "/dms?new=1".to_owned()),
        ("GET", "/chats".to_owned(), "/dms".to_owned()),
        ("GET", "/lenses?id=engineering".to_owned(), "/dna?id=engineering".to_owned()),
        ("GET", "/lenses/export".to_owned(), "/dna/export".to_owned()),
        ("HEAD", "/lenses".to_owned(), "/dna".to_owned()),
    ] {
        let response = request(&state, method, &old, "");
        assert_eq!(status_and_location(&response), ("301 Moved Permanently", Some(new.as_str())), "{method} {old}");
    }
    for (old, new) in [
        (format!("/chat/{id}/messages"), format!("/dm/{id}/messages")),
        (format!("/chat/{id}/project"), format!("/dm/{id}/project")),
        (format!("/chat/{id}/lenses"), format!("/dm/{id}/lenses")),
        ("/chats".to_owned(), "/dms".to_owned()),
        ("/chat".to_owned(), "/dms".to_owned()),
        ("/lenses/save?from=form".to_owned(), "/dna/save?from=form".to_owned()),
    ] {
        let response = request(&state, "POST", &old, "message=hello");
        assert_eq!(status_and_location(&response), ("308 Permanent Redirect", Some(new.as_str())), "POST {old}");
    }
    // Earlier history pages open the one History page with an equivalent filter.
    for (old, new) in [
        ("/chats/history".to_owned(), "/bots/history".to_owned()),
        (format!("/chats/history?older={id}&x=1"), format!("/bots/history?older={id}")),
        ("/bots/history/runs".to_owned(), "/bots/history?expand=1".to_owned()),
        ("/bots/history/runs?older=r-1".to_owned(), "/bots/history?expand=1".to_owned()),
    ] {
        let response = request(&state, "GET", &old, "");
        assert_eq!(status_and_location(&response), ("301 Moved Permanently", Some(new.as_str())), "GET {old}");
    }
    assert_eq!(status_and_location(&request(&state, "GET", "/?view=runs", "")).1, Some("/bots/history?expand=1"));
    // The old gene-library view goes to its page.
    assert_eq!(status_and_location(&request(&state, "GET", "/?view=lenses", "")).1, Some("/dna"));
}

#[test]
fn current_names_serve_and_pages_link_only_to_current_names() {
    let (state, id) = fixture();
    let pages = [
        "/".to_owned(),
        "/?view=agents".to_owned(),
        "/?view=rooms".to_owned(),
        "/?view=tools".to_owned(),
        "/?view=providers".to_owned(),
        "/knowledge".to_owned(),
        "/bots/history".to_owned(),
        "/dms?new=1".to_owned(),
        "/dms?discuss=tools".to_owned(),
        "/dna".to_owned(),
        "/dna?new=1".to_owned(),
        format!("/dm/{id}"),
        format!("/dm/{id}/transcript"),
        format!("/dm/{id}/requests"),
        format!("/dm/{id}/artifacts"),
        format!("/dm/{id}/memory"),
        format!("/dm/{id}/memory/node"),
        format!("/dm/{id}/project"),
    ];
    for page in &pages {
        let response = request(&state, "GET", page, "");
        assert_eq!(status_and_location(&response).0, "200 OK", "GET {page}: {response}");
        let links = local_links(&response);
        assert!(!links.is_empty() || page.ends_with("/requests") || page.ends_with("/transcript"), "{page} has no links");
        for link in links {
            let path = link.split(['?', '#']).next().unwrap();
            assert_eq!(url_names::renamed(path), None, "{page} links to legacy {link}");
            assert!(url_names::history(path, "").is_none(), "{page} links to legacy {link}");
        }
    }
    let dm = request(&state, "GET", &format!("/dm/{id}"), "");
    assert!(dm.contains(&format!("src='/dm/{id}/transcript#latest'")) && dm.contains(&format!("action='/dm/{id}/messages'")));
    let dna = request(&state, "GET", "/dna", "");
    assert!(dna.contains("action='/dna/save'") && dna.contains("href='/dna/export'"));
    // A new DM posted to the current list lands on its `/dm/ID` page.
    let created = request(&state, "POST", "/dms", "message=");
    assert!(status_and_location(&created).1.is_some_and(|location| location.starts_with("/dm/c-")), "{created}");
}
