use super::*;
use std::io::{BufRead, BufReader};

#[test]
fn streams_and_slow_clients_share_bounded_admission_and_cancel_on_shutdown() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(
        std::env::temp_dir().join(format!("control-http-cap-{}-{stamp}", std::process::id())),
    );
    fs::create_dir(&fixture.0).unwrap();
    assert!(
        fixture
            .command()
            .args(["config", "init"])
            .status()
            .unwrap()
            .success()
    );
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reservation.local_addr().unwrap();
    drop(reservation);
    let mut server = Server(
        fixture
            .command()
            .arg("serve")
            .env("CONTROL_ADDR", addr.to_string())
            .spawn()
            .unwrap(),
    );
    let start = Instant::now();
    while !healthy(addr) {
        assert!(server.0.try_wait().unwrap().is_none());
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }

    // Authenticate through the application, never reading credential files.
    let link = fixture
        .command()
        .args(["open", "--link"])
        .env("CONTROL_ADDR", addr.to_string())
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(link.status.success());
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .resolve("localhost", addr)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let response = client
        .get(String::from_utf8(link.stdout).unwrap().trim())
        .send()
        .unwrap_or_else(|_| panic!("fixture sign-in failed"));
    assert_eq!(response.status(), 303);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let mut streams = Vec::new();
    for _ in 0..16 {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(
            stream,
            "GET /events HTTP/1.1\r\nHost: localhost:{}\r\nCookie: {cookie}\r\n\r\n",
            addr.port()
        )
        .unwrap();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).unwrap();
        assert_eq!(line, "HTTP/1.1 200 OK\r\n");
        streams.push(stream);
    }
    assert!(
        healthy(addr),
        "live streams must leave ordinary request capacity"
    );
    let mut slow = Vec::new();
    for _ in 0..16 {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_write_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: local")
            .unwrap();
        slow.push(stream);
    }
    // The 33rd connection must be rejected without reading any request bytes.
    let excess = TcpStream::connect(addr).unwrap();
    excess
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut line = String::new();
    BufReader::new(excess).read_line(&mut line).unwrap();
    assert_eq!(line, "HTTP/1.1 503 Service Unavailable\r\n");

    // Closing one slow peer frees a slot, without waiting for its 10 s deadline.
    drop(slow.pop());
    let start = Instant::now();
    while !healthy(addr) {
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "capacity did not recover"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // Trickle bytes throughout shutdown: per-read timeouts alone cannot stop it.
    let trickle = std::thread::spawn(move || {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(12) {
            let mut alive = false;
            for stream in &mut slow {
                alive |= stream.write_all(b"x").is_ok();
            }
            if !alive {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let start = Instant::now();
    assert_eq!(
        unsafe { libc::kill(server.0.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            assert!(status.success(), "server did not exit cleanly: {status}");
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "streams or trickled requests blocked shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    trickle.join().unwrap();
    drop(streams);
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.0.join("state/state.json")).unwrap()).unwrap();
    assert!(
        state["runtime"]["runs"].as_array().unwrap().is_empty(),
        "transport admission must not create agent work"
    );
}
