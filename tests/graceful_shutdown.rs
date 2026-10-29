//! Real server-process signal checks, with isolated placeholder-only configuration.
use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Fixture {
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_control"));
        command
            .env_clear()
            .env("HOME", &self.0)
            .env("CONTROL_CONFIG_DIR", self.0.join("config"))
            .env("CONTROL_STATE_DIR", self.0.join("state"))
            .current_dir(&self.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
}
fn healthy(addr: SocketAddr) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(100)) else {
        return false;
    };
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    if write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n",
        addr.port()
    )
    .is_err()
    {
        return false;
    }
    let mut body = String::new();
    stream.read_to_string(&mut body).is_ok() && body.contains("Control is running.")
}

#[test]
fn term_and_interrupt_save_state_and_release_the_single_writer_lock() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(
        std::env::temp_dir().join(format!("control-signals-{}-{stamp}", std::process::id())),
    );
    fs::create_dir(&fixture.0).unwrap();
    assert!(fixture
        .command()
        .args(["config", "init"])
        .status()
        .unwrap()
        .success());
    // Restart the same node after each signal: the second start must acquire its state lock.
    for signal in [libc::SIGTERM, libc::SIGINT] {
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
            assert!(
                server.0.try_wait().unwrap().is_none(),
                "server exited before becoming healthy"
            );
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "server did not become healthy"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            unsafe { libc::kill(server.0.id() as libc::pid_t, signal) },
            0
        );
        let start = Instant::now();
        loop {
            if let Some(status) = server.0.try_wait().unwrap() {
                assert!(status.success(), "server did not exit cleanly: {status}");
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "idle shutdown did not finish promptly"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(!healthy(addr));
        // This is synthetic node state; no operator/provider credential is read.
        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.0.join("state/state.json")).unwrap()).unwrap();
        assert!(state["runtime"]["runs"].as_array().unwrap().is_empty());
    }
}
