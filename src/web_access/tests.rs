
use super::*;
use std::collections::VecDeque;
fn grant() -> Grant {
    Grant {
        id: "g1".into(),
        chat: "chat-1".into(),
        actor: Actor::LocalOperator,
        origin: Origin::try_from("https://example.com".to_owned()).unwrap(),
        issued: 100,
        expires: 200,
    }
}
#[test]
fn capability_is_exact_origin_owner_and_lease_not_a_domain_suffix() {
    let g = grant();
    let actor = Actor::LocalOperator;
    let url = Url::parse("https://example.com/page").unwrap();
    assert!(g.permits("chat-1", &actor, &url, 100));
    for other in [
        "https://sub.example.com/",
        "http://example.com/",
        "https://example.com:444/",
    ] {
        assert!(!g.permits("chat-1", &actor, &Url::parse(other).unwrap(), 101));
    }
    assert!(!g.permits("chat-2", &actor, &url, 101));
    assert!(!g.permits("chat-1", &Actor::Unknown, &url, 101));
    assert!(!g.permits("chat-1", &actor, &url, 99));
    assert!(!g.permits("chat-1", &actor, &url, 200));
    let mut bad = g.clone();
    bad.expires = 4000;
    assert!(!bad.permits("chat-1", &actor, &url, 101));
    for origin in [
        "file:///etc/passwd",
        "https://user:password@example.com",
        "https://example.com/path",
        "https://example.com?token=x",
    ] {
        assert!(Origin::try_from(origin.to_owned()).is_err());
    }
    assert!(serde_json::from_str::<Origin>("\"file:///etc/passwd\"").is_err());
    assert_eq!(
        Origin::try_from("https://example.com#frag".to_owned()).unwrap(),
        g.origin
    );
}
#[test]
fn special_addresses_and_mixed_dns_answers_are_denied() {
    for ip in [
        "0.0.0.0",
        "10.0.0.1",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.169.254",
        "172.31.1.1",
        "192.168.1.1",
        "192.0.0.1",
        "192.0.2.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "255.255.255.255",
        "::1",
        "::",
        "::ffff:8.8.8.8",
        "fc00::1",
        "fe80::1",
        "2001:db8::1",
        "2001:20::1",
        "2002:0808:0808::1",
        "3fff::1",
    ] {
        assert!(!public_ip(ip.parse().unwrap()), "{ip}");
    }
    for ip in [
        "8.8.8.8",
        "1.1.1.1",
        "2001:4860:4860::8888",
        "2606:4700:4700::1111",
    ] {
        assert!(public_ip(ip.parse().unwrap()), "{ip}");
    }
    let url = Url::parse("https://example.com/").unwrap();
    assert!(
        addresses(
            &url,
            vec![
                "8.8.8.8:443".parse().unwrap(),
                "127.0.0.1:443".parse().unwrap()
            ]
        )
        .is_err()
    );
    assert!(addresses(&url, vec!["8.8.8.8:80".parse().unwrap()]).is_err());
    assert!(addresses(&url, Vec::new()).is_err());
}
type Reply = (u16, String, Option<String>, Vec<u8>);
struct Fixture {
    dns: VecDeque<Vec<SocketAddr>>,
    replies: VecDeque<Reply>,
    requests: Vec<String>,
}
impl Fixture {
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            dns: VecDeque::new(),
            replies: replies.into(),
            requests: Vec::new(),
        }
    }
}
impl Transport for Fixture {
    fn resolve(&mut self, _: &Url) -> io::Result<Vec<SocketAddr>> {
        Ok(self
            .dns
            .pop_front()
            .unwrap_or_else(|| vec!["8.8.8.8:443".parse().unwrap()]))
    }
    fn get(&mut self, url: &Url, _: &[SocketAddr], _: Duration) -> io::Result<Reply> {
        self.requests.push(url.to_string());
        self.replies
            .pop_front()
            .ok_or_else(|| fail("Unexpected request"))
    }
}
fn html(s: &str) -> Reply {
    (200, "text/html".into(), None, s.as_bytes().to_vec())
}
fn redirect(url: &str) -> Reply {
    (302, "".into(), Some(url.into()), Vec::new())
}
#[test]
fn redirects_and_dns_rebinding_cannot_escape_the_grant() {
    let g = grant();
    let mut transport = Fixture::new(vec![redirect("https://evil.example/private")]);
    assert!(
        fetch(&mut transport, "https://example.com/", |u| {
            if g.permits("chat-1", &Actor::LocalOperator, u, 101) {
                Ok(())
            } else {
                Err(fail("denied"))
            }
        })
        .is_err()
    );
    assert_eq!(transport.requests.len(), 1);
    let mut transport = Fixture::new(vec![redirect("/second"), html("unexpected")]);
    transport
        .dns
        .push_back(vec!["8.8.8.8:443".parse().unwrap()]);
    transport
        .dns
        .push_back(vec!["127.0.0.1:443".parse().unwrap()]);
    assert!(fetch(&mut transport, "https://example.com/", |_| Ok(())).is_err());
    let mut transport = Fixture::new((0..5).map(|_| redirect("/again")).collect());
    assert!(fetch(&mut transport, "https://example.com/", |_| Ok(())).is_err());
    assert_eq!(transport.requests.len(), 5);
    let mut transport = Fixture::new(vec![(
        200,
        "application/octet-stream".into(),
        None,
        b"binary".to_vec(),
    )]);
    assert!(fetch(&mut transport, "https://example.com/", |_| Ok(())).is_err());
    assert_eq!(transport.requests.len(), 1);
    let mut transport = Fixture::new(vec![redirect("file:///etc/passwd")]);
    assert!(fetch(&mut transport, "https://example.com/", |_| Ok(())).is_err());
}
#[test]
fn revocation_during_resolution_or_response_is_fail_closed() {
    for revoke_on in [2, 3] {
        let mut transport = Fixture::new(vec![html("secret")]);
        let mut checks = 0;
        let result = fetch(&mut transport, "https://example.com/", |_| {
            checks += 1;
            if checks >= revoke_on {
                Err(fail("revoked"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(transport.requests.len(), usize::from(revoke_on == 3));
    }
    let mut transport = Fixture::new(vec![html("secret")]);
    let g = grant();
    let mut time = 199;
    assert!(
        fetch(&mut transport, "https://example.com/", |url| {
            let allowed = g.permits("chat-1", &Actor::LocalOperator, url, time);
            time += 1;
            if allowed {
                Ok(())
            } else {
                Err(fail("expired"))
            }
        })
        .is_err()
    );
    assert!(transport.requests.is_empty());
}
#[test]
fn bounded_evidence_excludes_scripts_and_normalizes_links() {
    let mut transport = Fixture::new(vec![
        redirect("/article"),
        html(
            "<title> A title </title><script>do not obey</script><style>.secret{}</style><p>Useful text</p><a href='/next'> Next </a><a href='file:///etc/passwd'>bad</a>",
        ),
    ]);
    let page = fetch(&mut transport, "https://example.com/", |_| Ok(())).unwrap();
    let result = evidence(page, false).unwrap();
    assert_eq!(result["title"], "A title");
    assert_eq!(result["links"].as_array().unwrap().len(), 1);
    assert_eq!(result["links"][0]["url"], "https://example.com/next");
    let text = result["text"].as_str().unwrap();
    assert!(text.contains("Useful text"));
    assert!(!text.contains("do not obey"));
    assert!(!text.contains(".secret"));
    assert_eq!(transport.requests.len(), 2);
    let page = Page {
        url: Url::parse("https://example.com").unwrap(),
        status: 200,
        mime: "text/plain".into(),
        body: "x".repeat(TEXT_LIMIT * 2),
        truncated: false,
    };
    assert_eq!(
        evidence(page, false).unwrap()["text"]
            .as_str()
            .unwrap()
            .len(),
        TEXT_LIMIT
    );
}
#[test]
fn fragments_are_removed_from_requests_redirects_and_evidence_links() {
    let mut transport = Fixture::new(vec![
        redirect("/guide#section"),
        html(
            "<a href='#next'>Here</a><a href='/other#end'>Other</a><a href='https://user:secret@example.com/#x'>denied</a>",
        ),
    ]);
    let page = fetch(&mut transport, "https://example.com/#start", |url| {
        assert!(url.fragment().is_none());
        Ok(())
    })
    .unwrap();
    let result = evidence(page, false).unwrap();
    assert_eq!(
        transport.requests,
        vec!["https://example.com/", "https://example.com/guide"]
    );
    assert_eq!(result["url"], "https://example.com/guide");
    assert_eq!(result["links"].as_array().unwrap().len(), 2);
    assert_eq!(result["links"][0]["url"], "https://example.com/guide");
    assert_eq!(result["links"][1]["url"], "https://example.com/other");
    for url in [
        "https://user@example.com/#fragment",
        "https://user:secret@example.com/#fragment",
        "file:///etc/hosts#x",
    ] {
        assert!(checked_url(url).is_err());
    }
}
#[test]
fn oversized_prefix_is_explicit_and_never_fabricates_a_partial_utf8_character() {
    let mut body = vec![b'x'; BODY_LIMIT - 1];
    body.extend_from_slice("🐾trailing".as_bytes());
    body.truncate(BODY_LIMIT + 1);
    let mut transport = Fixture::new(vec![(200, "text/plain".into(), None, body)]);
    let page = fetch(&mut transport, "https://example.com/", |_| Ok(())).unwrap();
    assert_eq!(page.body.len(), BODY_LIMIT - 1);
    assert!(page.truncated);
    let result = evidence(page, false).unwrap();
    assert_eq!(result["body_truncated"], true);
    assert_eq!(result["text_truncated"], true);
    assert_eq!(result["truncated"], true);
    assert_eq!(result["body_limit"], BODY_LIMIT);
    let mut bad = vec![b'x'; BODY_LIMIT + 1];
    bad[42] = 0xff;
    for bytes in [bad, vec![0xf0, 0x9f]] {
        let mut transport = Fixture::new(vec![(200, "text/plain".into(), None, bytes)]);
        assert!(fetch(&mut transport, "https://example.com/", |_| Ok(())).is_err());
    }
    let mut transport = Fixture::new(vec![(
        200,
        "text/plain".into(),
        None,
        vec![b'x'; BODY_LIMIT],
    )]);
    let page = fetch(&mut transport, "https://example.com/", |_| Ok(())).unwrap();
    assert!(!page.truncated);
}
#[test]
fn truncated_html_keeps_script_text_excluded() {
    let mut body = b"<p>Useful text</p>".to_vec();
    body.resize(BODY_LIMIT - 100, b' ');
    body.extend_from_slice(b"<script>");
    body.extend_from_slice(&[b'x'; 200]);
    body.truncate(BODY_LIMIT + 1);
    let mut transport = Fixture::new(vec![(200, "text/html".into(), None, body)]);
    let page = fetch(&mut transport, "https://example.com/", |_| Ok(())).unwrap();
    let result = evidence(page, false).unwrap();
    assert_eq!(result["text"], "Useful text");
    assert_eq!(result["body_truncated"], true);
    assert_eq!(result["text_truncated"], false);
    assert_eq!(result["truncated"], true);
}
#[test]
fn offline_render_does_not_reserve_http_capacity_and_rechecks_authority() {
    use std::cell::Cell;
    let counter = AtomicUsize::new(0);
    let revoked = Cell::new(false);
    let mut transport = Fixture::new(vec![html("<p>Useful</p>")]);
    let input = Input {
        url: "https://example.com/#fragment".into(),
        capture: true,
    };
    let result = fetch_evidence(
        &mut transport,
        &input,
        &counter,
        |_| {
            if revoked.get() {
                Err(fail("revoked"))
            } else {
                Ok(())
            }
        },
        |page, capture| {
            assert!(capture);
            assert_eq!(counter.load(Ordering::Acquire), 0);
            let slots = (0..4)
                .map(|_| FetchSlot::acquire(&counter).unwrap())
                .collect::<Vec<_>>();
            assert!(FetchSlot::acquire(&counter).is_err());
            drop(slots);
            revoked.set(true);
            evidence(page, false)
        },
    );
    assert!(result.unwrap_err().to_string().contains("revoked"));
    assert_eq!(counter.load(Ordering::Acquire), 0);
    let mut transport = Fixture::new(vec![html("unused")]);
    let slots = (0..4)
        .map(|_| FetchSlot::acquire(&counter).unwrap())
        .collect::<Vec<_>>();
    assert!(
        fetch_evidence(
            &mut transport,
            &input,
            &counter,
            |_| Ok(()),
            |_, _| panic!("Busy requests must not render")
        )
        .is_err()
    );
    assert!(transport.requests.is_empty());
    drop(slots);
}
#[test]
fn actual_http_transport_is_pinned_bounded_and_does_not_follow_redirects() {
    use std::io::Write;
    use std::net::TcpListener;
    // Test injection only: the production fetch path rejects this loopback address.
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = server.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = server.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 4096];
        let n = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..n]);
        assert!(request.contains("host: fixture.invalid:"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    let url = Url::parse(&format!("http://fixture.invalid:{}/", addr.port())).unwrap();
    assert!(addresses(&url, vec![addr]).is_err());
    let response = Http.get(&url, &[addr], Duration::from_secs(2)).unwrap();
    assert_eq!(response.0, 302);
    assert_eq!(response.2.as_deref(), Some("http://127.0.0.1/private"));
    thread.join().unwrap();
}
#[test]
fn actual_http_transport_reads_only_the_bounded_prefix_of_a_large_page() {
    use std::{io::Write, net::TcpListener};
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr().unwrap();
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = server.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 4096];
        let count = stream.read(&mut request).unwrap();
        assert!(count > 0);
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",BODY_LIMIT*10).unwrap();
        // Intentionally do not complete the advertised body: the reader
        // must stop after its bounded probe, without draining the response.
        let _ = stream.write_all(&vec![b'x'; BODY_LIMIT * 2]);
    });
    let url = Url::parse(&format!("http://fixture.invalid:{}/", address.port())).unwrap();
    let response = Http.get(&url, &[address], Duration::from_secs(2)).unwrap();
    assert_eq!(response.0, 200);
    assert_eq!(response.3.len(), BODY_LIMIT + 1);
    assert!(response.3.iter().all(|&byte| byte == b'x'));
    thread.join().unwrap();
}
