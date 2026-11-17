use super::*;
use crate::ports::tests::fixture;

/// A plain TCP echo upstream standing in for an approved TLS host.
fn echo() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for socket in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut reading = socket.try_clone().unwrap();
                let mut writing = socket;
                let _ = io::copy(&mut reading, &mut writing);
            });
        }
    });
    address
}
/// A service with one approved egress grant to `upstream`, and its proxy key.
fn approved(upstream: SocketAddr) -> (Arc<Mutex<State>>, Key) {
    let (mut state, run, _) = fixture();
    let input = Input {
        service_id: ResourceId::try_from("svc-a".to_owned()).unwrap(),
        name: Name::try_from("upstream".to_owned()).unwrap(),
        direction: Direction::Egress,
        service_port: NonZeroU16::new(31000).unwrap(),
        target: Some(Endpoint { host: "127.0.0.1".into(), port: NonZeroU16::new(upstream.port()).unwrap() }),
        peer_port_id: None,
    };
    admit(&mut state, &run, "attempt-a", "nonce-a", input, "node-a".to_owned().try_into().unwrap(), |_| Ok(())).unwrap();
    let id = state.ports[0].id.as_str().to_owned();
    assert!(env(&state, &state.services[0]).is_empty(), "no proxy settings before approval");
    approve(&mut state, &id, 60, true, Some(upstream), now(), |_| Ok(())).unwrap();
    let key = Key::of(&state.ports[0]);
    (Arc::new(Mutex::new(state)), key)
}
/// Sends `request` through the proxy handler and returns the client end.
fn proxy(shared: &Arc<Mutex<State>>, key: &Key, request: &str) -> (TcpStream, JoinHandle<()>) {
    let (mut client, server) = pair().unwrap();
    let (shared, key) = (shared.clone(), key.clone());
    let task = thread::spawn(move || {
        let _ = handle(&shared, &key, &AtomicBool::new(false), server);
    });
    client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    (client, task)
}
fn response(client: &mut TcpStream) -> String {
    let mut text = String::new();
    let _ = client.read_to_string(&mut text);
    text
}
fn read_exactly(client: &mut TcpStream, size: usize) -> String {
    let mut bytes = vec![0; size];
    client.read_exact(&mut bytes).unwrap();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn connect_to_an_approved_host_tunnels_bytes_end_to_end() {
    let upstream = echo();
    let (shared, key) = approved(upstream);
    let port = upstream.port();
    let (mut client, task) = proxy(&shared, &key, &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\nearly"));
    let established = "HTTP/1.1 200 Connection established\r\n\r\n";
    assert_eq!(read_exactly(&mut client, established.len()), established);
    assert_eq!(read_exactly(&mut client, 5), "early", "bytes sent with the head are forwarded");
    client.write_all(b"\x16\x03\x01 opaque tls bytes").unwrap();
    assert_eq!(read_exactly(&mut client, 20), "\x16\x03\x01 opaque tls bytes");
    client.shutdown(Shutdown::Write).unwrap();
    assert_eq!(response(&mut client), "", "half-close reaches the upstream and back");
    task.join().unwrap();
}

#[test]
fn unapproved_hosts_get_a_403_naming_the_fix() {
    let upstream = echo();
    let (shared, key) = approved(upstream);
    for request in [
        "CONNECT registry.npmjs.org:443 HTTP/1.1\r\n\r\n".to_owned(),
        format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", upstream.port() as u32 + 1),
    ] {
        let (mut client, task) = proxy(&shared, &key, &request);
        let text = response(&mut client);
        assert!(text.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{text}");
        assert!(text.contains("Request an egress port for") && text.contains("control_port_request"), "{text}");
        task.join().unwrap();
    }
    let (mut client, _) = proxy(&shared, &key, "CONNECT registry.npmjs.org:443 HTTP/1.1\r\n\r\n");
    assert!(response(&mut client).contains("\"target\":{\"host\":\"registry.npmjs.org\",\"port\":443}"));
}

#[test]
fn plain_http_is_relayed_only_to_approved_port_80() {
    let upstream = echo();
    let (shared, key) = approved(upstream);
    let port = upstream.port();
    let (mut client, _) = proxy(&shared, &key, &format!("GET http://127.0.0.1:{port}/a HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"));
    let text = response(&mut client);
    assert!(text.starts_with("HTTP/1.1 403") && text.contains("only to port 80"), "{text}");
    let (mut client, _) = proxy(&shared, &key, "GET http://example.com/a HTTP/1.1\r\nHost: example.com\r\n\r\n");
    assert!(response(&mut client).contains("example.com:80 is not an approved egress target"));
    let (mut client, _) = proxy(&shared, &key, "GET /a HTTP/1.1\r\nHost: example.com\r\n\r\n");
    assert!(response(&mut client).starts_with("HTTP/1.1 400"));
    let request = parse(b"GET http://Example.com/a?b HTTP/1.1\r\nHost: example.com\r\nProxy-Connection: keep-alive\r\n\r\n").unwrap();
    assert_eq!((request.host.as_str(), request.port), ("Example.com", 80));
    assert_eq!(request.forward.unwrap(), b"GET /a?b HTTP/1.1\r\nHost: example.com\r\n\r\n");
    assert_eq!(parse(b"CONNECT [::1]:443 HTTP/1.1\r\n\r\n").unwrap(), Request { host: "::1".into(), port: 443, forward: None });
    assert!(parse(b"CONNECT example.com HTTP/1.1\r\n\r\n").is_err());
    assert!(parse(b"GET http://user@example.com/ HTTP/1.1\r\n\r\n").is_err());
}

#[test]
fn revoked_grants_refuse_new_connects_and_close_open_tunnels() {
    let upstream = echo();
    let (shared, key) = approved(upstream);
    let port = upstream.port();
    let (mut open, task) = proxy(&shared, &key, &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"));
    let established = "HTTP/1.1 200 Connection established\r\n\r\n";
    assert_eq!(read_exactly(&mut open, established.len()), established);
    shared.lock().unwrap().ports[0].status = Status::Stopping;
    let (mut refused, _) = proxy(&shared, &key, &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"));
    assert!(response(&mut refused).starts_with("HTTP/1.1 403"));
    // The open tunnel follows the port relay: revocation closes it.
    assert_eq!(response(&mut open), "");
    task.join().unwrap();
    let state = shared.lock().unwrap();
    assert!(grants(&state, &key, now()).next().is_none());
    assert!(env(&state, &state.services[0]).is_empty(), "settings leave with the last approved egress");
}

#[test]
fn proxy_settings_and_usage_appear_only_with_approved_egress() {
    let (shared, _) = approved(echo());
    let state = shared.lock().unwrap();
    let service = &state.services[0];
    let names = env(&state, service).iter().map(|(name, _)| *name).collect::<Vec<_>>();
    assert_eq!(names, ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy", "NO_PROXY", "no_proxy"]);
    assert!(env(&state, service).contains(&("HTTPS_PROXY", "http://127.0.0.1:3129")));
    assert!(exports(&state, service).starts_with("export HTTPS_PROXY=http://127.0.0.1:3129 "));
    let usage = state.ports[0].summary()["usage"].as_str().unwrap().to_owned();
    assert!(usage.contains("HTTPS_PROXY") && usage.contains("npm ci, pip, cargo, git and curl") && usage.contains("--connect-to"), "{usage}");
    let (fresh, _, _) = fixture();
    assert!(env(&fresh, &fresh.services[0]).is_empty() && exports(&fresh, &fresh.services[0]).is_empty());
}

#[test]
fn the_proxy_port_is_reserved() {
    let (mut state, run, _) = fixture();
    let input = Input {
        service_id: ResourceId::try_from("svc-a".to_owned()).unwrap(),
        name: Name::try_from("clash".to_owned()).unwrap(),
        direction: Direction::Ingress,
        service_port: PROXY_PORT,
        target: None,
        peer_port_id: None,
    };
    let error = admit(&mut state, &run, "attempt-a", "nonce-a", input, "node-a".to_owned().try_into().unwrap(), |_| Ok(())).unwrap_err();
    assert!(error.to_string().contains("reserved for Control's egress proxy"), "{error}");
}

/// The container listener and host ends of a multiplex relay over a local
/// socket, standing in for `listen-multi` over engine stdio.
struct Relay {
    listener: Arc<Transport>,
    host: Arc<Transport>,
    incoming: std::sync::mpsc::Receiver<Incoming>,
}
impl Relay {
    fn new() -> Self {
        let (a, b) = pair().unwrap();
        let (listener, _, _) = Transport::start(a.try_clone().unwrap(), a, false);
        let (host, incoming, _) = Transport::start(b.try_clone().unwrap(), b, true);
        Self { listener, host, incoming }
    }
    /// A service-side connection; `serve` decides what the host does with it.
    fn connect(&self, serve: impl FnOnce(Incoming, Arc<Transport>) + Send + 'static) -> TcpStream {
        let (client, socket) = pair().unwrap();
        self.listener.open(socket).unwrap();
        let incoming = self.incoming.recv_timeout(Duration::from_secs(2)).unwrap();
        let host = self.host.clone();
        thread::spawn(move || serve(incoming, host));
        client.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        client
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.listener.stop();
        self.host.stop();
    }
}
/// A client through the proxy session, tunnelled to `upstream`.
fn tunnelled(relay: &Relay, shared: &Arc<Mutex<State>>, key: &Key, upstream: SocketAddr) -> TcpStream {
    let (shared, key) = (shared.clone(), key.clone());
    let mut client = relay.connect(move |incoming, link| session(&shared, &key, &AtomicBool::new(false), incoming, link));
    client.write_all(format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", upstream.port()).as_bytes()).unwrap();
    let established = "HTTP/1.1 200 Connection established\r\n\r\n";
    assert_eq!(read_exactly(&mut client, established.len()), established);
    client
}
/// A client on the existing raw egress path: the stream attaches straight to `upstream`.
fn raw(relay: &Relay, upstream: SocketAddr) -> TcpStream {
    relay.connect(move |incoming, link| {
        let _ = link.attach(incoming, TcpStream::connect(upstream).unwrap());
    })
}

#[test]
fn relayed_streams_reach_the_proxy_and_tunnel_through_the_multiplex_transport() {
    let upstream = echo();
    let (shared, key) = approved(upstream);
    let relay = Relay::new();
    let mut client = tunnelled(&relay, &shared, &key, upstream);
    client.write_all(b"through the relay").unwrap();
    assert_eq!(read_exactly(&mut client, 17), "through the relay");
    let (shared2, key2) = (shared.clone(), key.clone());
    let mut denied = relay.connect(move |incoming, link| session(&shared2, &key2, &AtomicBool::new(false), incoming, link));
    denied.write_all(b"CONNECT github.com:443 HTTP/1.1\r\n\r\n").unwrap();
    assert!(response(&mut denied).contains("Request an egress port for github.com:443"));
}

/// Throughput and round-trip latency of the proxy tunnel against the raw
/// egress relay, both over the same in-process multiplex transport. Run with
/// `cargo test --release --bin control proxy_relay_benchmark -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark; prints throughput and latency"]
fn proxy_relay_benchmark() {
    let upstream = echo();
    let (shared, key) = approved(upstream);
    let relay = Relay::new();
    const BYTES: usize = 64 << 20;
    const ROUNDS: usize = 2000;
    let measure = |name: &str, mut client: TcpStream| {
        let mut writer = client.try_clone().unwrap();
        let started = Instant::now();
        let sending = thread::spawn(move || {
            let block = vec![7u8; 64 << 10];
            for _ in 0..BYTES / block.len() {
                writer.write_all(&block).unwrap();
            }
        });
        let mut buffer = vec![0u8; 64 << 10];
        let mut received = 0;
        while received < BYTES {
            received += client.read(&mut buffer).unwrap();
        }
        sending.join().unwrap();
        let throughput = BYTES as f64 / (1 << 20) as f64 / started.elapsed().as_secs_f64();
        let started = Instant::now();
        for _ in 0..ROUNDS {
            client.write_all(b"x").unwrap();
            client.read_exact(&mut buffer[..1]).unwrap();
        }
        let latency = started.elapsed().as_micros() as f64 / ROUNDS as f64;
        println!("{name}: {throughput:.0} MiB/s echoed, {latency:.0} µs round trip");
    };
    // Each new path connection is setup cost, not hot path; measure it too.
    let started = Instant::now();
    for _ in 0..50 {
        drop(tunnelled(&relay, &shared, &key, upstream));
    }
    println!("proxy CONNECT setup: {:.0} µs", started.elapsed().as_micros() as f64 / 50.0);
    measure("raw relay", raw(&relay, upstream));
    measure("proxy tunnel", tunnelled(&relay, &shared, &key, upstream));
}
