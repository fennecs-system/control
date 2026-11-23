use super::*;
use control_inject::{ANTHROPIC, AttemptId, OPENAI, Shape, Token};
use std::{
    net::{SocketAddr, TcpListener},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};

const SECRET: &[u8] = b"sk-ant-api03-CANARY-SECRET-0123";
const TOKEN: [u8; 16] = *b"canary-token-016";

/// Lends the canary key.
struct Canary;
impl SecretSource<&'static str> for Canary {
    fn with_secret<R>(&self, account: &&'static str, f: impl FnOnce(&[u8]) -> R) -> io::Result<R> {
        assert_eq!(*account, "claude+work");
        Ok(f(SECRET))
    }
}
fn placeholder(shape: Shape) -> String {
    let cap = AttemptCap::new(AttemptId("a".into()), (), Token::from_bytes(TOKEN));
    cap.placeholder(shape).expose().to_owned()
}
fn hex() -> String {
    TOKEN.iter().map(|b| format!("{b:02x}")).collect()
}
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// What a scripted provider does once it has read a whole request.
type Script = dyn Fn(&mut TcpStream) + Send + Sync;
/// A plain-TCP stand-in for a provider: it records each request (head and
/// body) and answers with `script`.
struct Provider {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
}
impl Provider {
    fn start(script: impl Fn(&mut TcpStream) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (seen, script): (_, Arc<Script>) = (requests.clone(), Arc::new(script));
        thread::spawn(move || {
            for mut socket in listener.incoming().flatten() {
                let (seen, script) = (seen.clone(), script.clone());
                thread::spawn(move || {
                    let request = read_request(&mut socket);
                    seen.lock().unwrap().push(request);
                    script(&mut socket);
                });
            }
        });
        Self { address, requests }
    }
    fn requests(&self) -> Vec<Vec<u8>> {
        self.requests.lock().unwrap().clone()
    }
}
fn read_request(socket: &mut TcpStream) -> Vec<u8> {
    let mut data = Vec::new();
    let mut byte = [0; 1];
    while !data.ends_with(b"\r\n\r\n") && socket.read(&mut byte).unwrap_or(0) == 1 {
        data.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&data).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .map_or(0, |n| n.trim().parse().unwrap());
    let mut body = vec![0; length];
    socket.read_exact(&mut body).unwrap();
    data.extend(body);
    data
}
const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: hello\n\n";
fn answer(bytes: &'static [u8]) -> impl Fn(&mut TcpStream) + Send + Sync + 'static {
    move |socket| socket.write_all(bytes).unwrap()
}

/// Connects every request to the provider whatever the origin, counting connections.
struct Loopback {
    address: SocketAddr,
    connects: AtomicUsize,
    origins: Mutex<Vec<Origin>>,
}
impl Upstream for Loopback {
    type Conn = TcpStream;
    fn connect(&self, origin: &Origin) -> io::Result<TcpStream> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        self.origins.lock().unwrap().push(*origin);
        TcpStream::connect(self.address)
    }
}

type TestRelay = Arc<Relay<&'static str, Canary, Loopback>>;
fn audit_path() -> PathBuf {
    let directory = crate::test_support::scratch("inject-audit");
    fs::create_dir_all(&directory).unwrap();
    directory.join("secrets-audit.jsonl")
}
fn relay_for(provider: &Provider, limits: Limits, timing: Timing, audit: &Path) -> TestRelay {
    let cap = AttemptCap::new(AttemptId("attempt-7".into()), "claude+work", Token::from_bytes(TOKEN));
    let upstream = Loopback {
        address: provider.address,
        connects: AtomicUsize::new(0),
        origins: Mutex::new(Vec::new()),
    };
    let log = AuditFile::at(audit.to_path_buf(), Retention::DEFAULT);
    Arc::new(Relay::new(&ANTHROPIC, cap, Canary, upstream, log, limits, timing))
}
fn connected(relay: &TestRelay) -> TcpStream {
    let (client, server) = super::super::proxy::pair().unwrap();
    let relay = relay.clone();
    thread::spawn(move || relay.serve(server));
    client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    client
}
/// Sends `request` and returns every byte the relay wrote back.
fn ask(relay: &TestRelay, request: &[u8]) -> Vec<u8> {
    let mut client = connected(relay);
    client.write_all(request).unwrap();
    let mut response = Vec::new();
    let _ = client.read_to_end(&mut response);
    response
}
fn messages(body: &str) -> Vec<u8> {
    let key = placeholder(Shape::Plain);
    format!(
        "POST /v1/messages?beta=true HTTP/1.1\r\nHost: 127.0.0.1:3130\r\nAuthorization: Bearer {key}\r\nx-api-key: {key}\r\nanthropic-version: 2023-06-01\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}
fn status(response: &[u8]) -> String {
    String::from_utf8_lossy(&response[..response.len().min(12)]).into_owned()
}
fn audit_lines(path: &PathBuf) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn the_provider_gets_the_key_and_the_client_never_does() {
    let provider = Provider::start(answer(OK));
    let audit = audit_path();
    let relay = relay_for(&provider, Limits::DEFAULT, Timing::DEFAULT, &audit);
    let body = r#"{"model":"claude","stream":true}"#;
    let response = ask(&relay, &messages(body));
    assert_eq!(response, OK, "the response is relayed as sent");
    let requests = provider.requests();
    let [sent] = &requests[..] else { panic!("{requests:?}") };
    let text = String::from_utf8_lossy(sent);
    assert!(contains(sent, SECRET), "{text}");
    assert!(
        text.contains("\r\nx-api-key: sk-ant-api03-CANARY-SECRET-0123\r\n"),
        "{text}"
    );
    assert!(text.starts_with("POST /v1/messages?beta=true HTTP/1.1\r\nHost: api.anthropic.com\r\n"));
    assert!(text.ends_with(&format!("\r\n\r\n{body}")), "exactly the body: {text}");
    assert!(
        !contains(&sent.to_ascii_lowercase(), hex().as_bytes()),
        "no placeholder upstream"
    );
    assert!(!text.to_ascii_lowercase().contains("authorization"));
    assert_eq!(*relay.upstream.origins.lock().unwrap(), [ANTHROPIC.origin]);

    // Refused requests, relayed errors and everything else the client reads.
    let wrong = String::from_utf8(messages("{}"))
        .unwrap()
        .replace(&hex(), &"0".repeat(32));
    let route = String::from_utf8(messages("{}"))
        .unwrap()
        .replace("/v1/messages", "/v1/files");
    let answers = [
        response,
        ask(&relay, wrong.as_bytes()),
        ask(&relay, route.as_bytes()),
        ask(&relay, b"GET / HTTP/1.1\r\n\r\n"),
    ];
    assert!(answers[1].starts_with(b"HTTP/1.1 403"), "{}", status(&answers[1]));
    assert!(answers[2].starts_with(b"HTTP/1.1 403"), "{}", status(&answers[2]));
    assert!(answers[3].starts_with(b"HTTP/1.1 400"), "{}", status(&answers[3]));
    assert!(answers.iter().all(|bytes| !contains(bytes, SECRET)));
    assert_eq!(
        relay.upstream.connects.load(Ordering::SeqCst),
        1,
        "refusals open no connection"
    );
}

#[test]
fn a_provider_error_is_relayed_without_the_key() {
    let provider = Provider::start(answer(
        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 27\r\n\r\n{\"error\":\"invalid x-api-key\"}",
    ));
    let audit = audit_path();
    let relay = relay_for(&provider, Limits::DEFAULT, Timing::DEFAULT, &audit);
    let response = ask(&relay, &messages("{}"));
    assert!(response.starts_with(b"HTTP/1.1 401"));
    assert!(!contains(&response, SECRET));
    assert_eq!(audit_lines(&audit)[0]["status"], 401);
}

#[test]
fn server_sent_events_reach_the_client_before_the_provider_finishes() {
    let (open, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let provider = Provider::start(move |socket| {
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\nevent: one\n\n")
            .unwrap();
        gate.lock().unwrap().recv().unwrap();
        socket.write_all(b"event: two\n\n").unwrap();
        done.store(true, Ordering::SeqCst);
    });
    let relay = relay_for(&provider, Limits::DEFAULT, Timing::DEFAULT, &audit_path());
    let mut client = connected(&relay);
    client.write_all(&messages("{}")).unwrap();
    let mut seen = Vec::new();
    let mut chunk = [0; 1024];
    while !contains(&seen, b"event: one\n\n") {
        let n = client.read(&mut chunk).unwrap();
        assert!(n > 0, "the first event arrives while the provider is still streaming");
        seen.extend_from_slice(&chunk[..n]);
    }
    assert!(!finished.load(Ordering::SeqCst), "the provider has not finished yet");
    open.send(()).unwrap();
    client.read_to_end(&mut seen).unwrap();
    assert!(seen.ends_with(b"event: one\n\nevent: two\n\n"));
}

/// A provider that holds each response open until the test lets it go.
fn held() -> (Provider, mpsc::Sender<()>) {
    let (release, gate) = mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let provider = Provider::start(move |socket| {
        socket.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
        let _ = gate.lock().unwrap().recv();
    });
    (provider, release)
}

#[test]
fn each_bound_trips_with_its_status() {
    // Concurrent streams: the second is refused while the first is open.
    let (provider, release) = held();
    let one = Limits {
        streams: 1,
        ..Limits::DEFAULT
    };
    let audit = audit_path();
    let relay = relay_for(&provider, one, Timing::DEFAULT, &audit);
    let mut first = connected(&relay);
    first.write_all(&messages("{}")).unwrap();
    let mut started = [0; 17];
    first.read_exact(&mut started).unwrap();
    assert_eq!(status(&ask(&relay, &messages("{}"))), "HTTP/1.1 429");
    release.send(()).unwrap();
    let _ = first.read_to_end(&mut Vec::new());

    // Requests per minute.
    let provider = Provider::start(answer(OK));
    let rate = Limits {
        per_minute: 1,
        ..Limits::DEFAULT
    };
    let relay = relay_for(&provider, rate, Timing::DEFAULT, &audit);
    assert_eq!(status(&ask(&relay, &messages("{}"))), "HTTP/1.1 200");
    assert_eq!(status(&ask(&relay, &messages("{}"))), "HTTP/1.1 429");

    // Total bytes: a response past the attempt's allowance is cut, and the
    // next request is refused.
    let big: &'static [u8] = Box::leak(
        [&b"HTTP/1.1 200 OK\r\n\r\n"[..], &[b'x'; 64 * 1024]]
            .concat()
            .into_boxed_slice(),
    );
    let provider = Provider::start(answer(big));
    let bytes = Limits {
        total_bytes: 8 * 1024,
        ..Limits::DEFAULT
    };
    let relay = relay_for(&provider, bytes, Timing::DEFAULT, &audit);
    let cut = ask(&relay, &messages("{}"));
    assert!(cut.len() < 8 * 1024, "{}", cut.len());
    assert_eq!(status(&ask(&relay, &messages("{}"))), "HTTP/1.1 429");

    // Body size, head size and head time, refused before any connection.
    let provider = Provider::start(answer(OK));
    let quick = Timing {
        head: Duration::from_millis(300),
        ..Timing::DEFAULT
    };
    let relay = relay_for(&provider, Limits::DEFAULT, quick, &audit);
    let key = placeholder(Shape::Plain);
    let huge = format!("POST /v1/messages HTTP/1.1\r\nHost: x\r\nx-api-key: {key}\r\nContent-Length: 33554433\r\n\r\n");
    assert_eq!(status(&ask(&relay, huge.as_bytes())), "HTTP/1.1 413");
    let long = format!("GET /v1/models HTTP/1.1\r\nX-Pad: {}", "a".repeat(17 * 1024));
    assert_eq!(status(&ask(&relay, long.as_bytes())), "HTTP/1.1 431");
    assert_eq!(
        status(&ask(&relay, b"GET /v1/models HTTP/1.1\r\nHost: x\r\n")),
        "HTTP/1.1 408"
    );
    let chunked =
        format!("POST /v1/messages HTTP/1.1\r\nHost: x\r\nx-api-key: {key}\r\nTransfer-Encoding: chunked\r\n\r\n");
    assert_eq!(status(&ask(&relay, chunked.as_bytes())), "HTTP/1.1 411");
    assert_eq!(relay.upstream.connects.load(Ordering::SeqCst), 0);

    let refusals: Vec<String> = audit_lines(&audit)
        .iter()
        .filter_map(|line| line["refusal"].as_str().map(str::to_owned))
        .collect();
    for expected in [
        "too_many_streams",
        "rate_limited",
        "byte_budget_spent",
        "body_too_large",
        "head_too_large",
        "head_timeout",
        "transfer_encoding",
    ] {
        assert!(refusals.iter().any(|r| r == expected), "{expected} in {refusals:?}");
    }
}

#[test]
fn the_audit_log_holds_metadata_only_and_stays_bounded() {
    let provider = Provider::start(answer(OK));
    let audit = audit_path();
    let relay = relay_for(&provider, Limits::DEFAULT, Timing::DEFAULT, &audit);
    ask(&relay, &messages(r#"{"prompt":"BODY-CANARY"}"#));
    let refused = String::from_utf8(messages("{}"))
        .unwrap()
        .replace("/v1/messages", "/v1/admin");
    ask(&relay, refused.as_bytes());
    let text = fs::read_to_string(&audit).unwrap();
    for canary in [
        String::from_utf8_lossy(SECRET).into_owned(),
        hex(),
        "BODY-CANARY".into(),
        "beta=true".into(),
        "anthropic-version".into(),
        "2023-06-01".into(),
    ] {
        assert!(!text.contains(&canary), "{canary} in {text}");
    }
    let lines = audit_lines(&audit);
    let keys: Vec<&String> = lines[0].as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "account",
            "attempt",
            "bytes_in",
            "bytes_out",
            "host",
            "method",
            "ms",
            "refusal",
            "route_class",
            "status",
            "time"
        ]
    );
    assert_eq!(lines[0]["attempt"], "attempt-7");
    assert_eq!(lines[0]["account"], "claude+work");
    assert_eq!(lines[0]["host"], "api.anthropic.com");
    assert_eq!(lines[0]["route_class"], "/v1/messages");
    assert_eq!(lines[0]["status"], 200);
    assert_eq!(lines[0]["bytes_out"], OK.len());
    assert!(lines[0]["refusal"].is_null());
    assert_eq!(lines[1]["refusal"], "route_not_allowed");
    assert_eq!(lines[1]["status"], 403);
    assert!(lines[1]["route_class"].is_null());
    assert_eq!(relay.upstream.connects.load(Ordering::SeqCst), 1);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(fs::metadata(&audit).unwrap().permissions().mode() & 0o777, 0o600);

    // Appending prunes: old lines go, and the log stays under its cap.
    let small = AuditFile::at(
        audit.clone(),
        Retention {
            max_age_seconds: 60,
            max_bytes: 1024,
        },
    );
    let record = |time| Record {
        time,
        attempt: "a".into(),
        account: "b".into(),
        method: None,
        host: "h".into(),
        route_class: None,
        status: None,
        bytes_in: 0,
        bytes_out: 0,
        ms: 0,
        refusal: None,
    };
    let now = crate::config::unix_seconds();
    (0..40).for_each(|_| small.append(&record(now)).unwrap());
    let size = fs::metadata(&audit).unwrap().len();
    assert!(size <= 1024, "{size}");
    small.append(&record(now + 3600)).unwrap();
    assert_eq!(audit_lines(&audit).len(), 1, "an hour later the rest has aged out");
}

/// The test CA and its `localhost` certificate (tests only; valid to 2126).
const CA: &[u8] = include_bytes!("testdata/test-ca.der");
const LEAF: &[u8] = include_bytes!("testdata/localhost.der");
const LEAF_KEY: &[u8] = include_bytes!("testdata/localhost-key.der");

/// A TLS provider on loopback with the test certificate.
fn tls_provider(requests: Arc<Mutex<Vec<Vec<u8>>>>) -> u16 {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(LEAF.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(LEAF_KEY.to_vec())),
        )
        .unwrap();
    let config = Arc::new(config);
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        for socket in listener.incoming().flatten() {
            let (config, requests) = (config.clone(), requests.clone());
            thread::spawn(move || {
                let connection = rustls::ServerConnection::new(config).unwrap();
                let mut stream = rustls::StreamOwned::new(connection, socket);
                let mut data = Vec::new();
                let mut byte = [0; 1];
                while !data.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
                    data.push(byte[0]);
                }
                if data.is_empty() {
                    return;
                }
                requests.lock().unwrap().push(data);
                let _ = stream.write_all(OK);
                stream.conn.send_close_notify();
                let _ = stream.flush();
            });
        }
    });
    port
}

#[test]
fn the_tls_upstream_reaches_only_a_trusted_fixed_origin() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let port = tls_provider(requests.clone());
    let rule: &'static InjectionRule = Box::leak(Box::new(InjectionRule {
        origin: Origin::https("localhost", port),
        routes: OPENAI.routes,
        ..OPENAI.clone()
    }));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls::pki_types::CertificateDer::from(CA.to_vec())).unwrap();
    let serve = |tls: Tls| {
        let cap = AttemptCap::new(AttemptId("attempt-9".into()), "claude+work", Token::from_bytes(TOKEN));
        let relay = Relay::new(
            rule,
            cap,
            Canary,
            tls,
            AuditFile::at(audit_path(), Retention::DEFAULT),
            Limits::DEFAULT,
            Timing::DEFAULT,
        );
        let (mut client, server) = super::super::proxy::pair().unwrap();
        let key = placeholder(Shape::OpenAiProject);
        client
            .write_all(format!("GET /v1/models HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {key}\r\n\r\n").as_bytes())
            .unwrap();
        relay.serve(server);
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        response
    };
    assert_eq!(serve(Tls::trusting(roots).unwrap()), OK);
    let sent = requests.lock().unwrap().clone();
    let [sent] = &sent[..] else { panic!("{sent:?}") };
    let text = String::from_utf8_lossy(sent);
    assert!(text.starts_with(&format!("GET /v1/models HTTP/1.1\r\nHost: localhost:{port}\r\n")));
    assert!(
        text.contains("authorization: Bearer sk-ant-api03-CANARY-SECRET-0123\r\n"),
        "{text}"
    );
    // The web PKI does not trust the test certificate: nothing is sent.
    let refused = serve(Tls::new().unwrap());
    assert_eq!(status(&refused), "HTTP/1.1 502");
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[test]
fn interleaved_streams_through_the_multiplex_relay_each_reach_the_edge() {
    let provider = Provider::start(answer(OK));
    let relay = relay_for(&provider, Limits::DEFAULT, Timing::DEFAULT, &audit_path());
    // The container's listener and the host's end of one multiplex relay.
    let (near, far) = super::super::proxy::pair().unwrap();
    let (container, _, _) = Transport::start(near.try_clone().unwrap(), near, false);
    let (host, incoming, _) = Transport::start(far.try_clone().unwrap(), far, true);
    let open = || {
        let (client, socket) = super::super::proxy::pair().unwrap();
        container.open(socket).unwrap();
        let connection = incoming.recv_timeout(Duration::from_secs(2)).unwrap();
        let (relay, link) = (relay.clone(), host.clone());
        thread::spawn(move || session(&relay, connection, link));
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        client
    };
    let (mut a, mut b) = (open(), open());
    let request = messages(r#"{"n":1}"#);
    let (first, rest) = request.split_at(40);
    a.write_all(first).unwrap();
    b.write_all(&messages(r#"{"n":2}"#)).unwrap();
    let mut answer_b = Vec::new();
    b.read_to_end(&mut answer_b).unwrap();
    a.write_all(rest).unwrap();
    let mut answer_a = Vec::new();
    a.read_to_end(&mut answer_a).unwrap();
    assert_eq!((answer_a.as_slice(), answer_b.as_slice()), (OK, OK));
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|r| contains(r, SECRET) && !contains(r, hex().as_bytes()))
    );
    assert!(contains(&requests[0], br#"{"n":2}"#), "b finished first");
    assert!(contains(&requests[1], br#"{"n":1}"#));
    container.stop();
    host.stop();
}
