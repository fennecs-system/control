//! Real server-process checks, with isolated placeholder-only configuration.
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
fn printed_browser_link_is_private_single_use_and_needs_no_browser_opener() {
    use std::os::unix::fs::PermissionsExt;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture =
        Fixture(std::env::temp_dir().join(format!("control-open-{}-{stamp}", std::process::id())));
    fs::create_dir(&fixture.0).unwrap();
    assert!(fixture
        .command()
        .args(["config", "init"])
        .status()
        .unwrap()
        .success());
    for args in [
        ["open", "--help"].as_slice(),
        ["open", "--link", "extra"].as_slice(),
    ] {
        let output = fixture.command().args(args).output().unwrap();
        assert_eq!(output.status.success(), args.contains(&"--help"));
        assert!(!fixture.0.join("state/operator-unlock").exists());
    }
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reservation.local_addr().unwrap();
    drop(reservation);
    let link = || {
        let output = fixture
            .command()
            .args(["open", "--link"])
            .env("CONTROL_ADDR", addr.to_string())
            .env("PATH", fixture.0.join("no-browser"))
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        assert!(output.status.success(), "link generation failed");
        let text = String::from_utf8(output.stdout).unwrap();
        let url = reqwest::Url::parse(text.trim()).expect("link must be a URL");
        assert!(
            url.scheme() == "http"
                && url.host_str() == Some("localhost")
                && url.port() == Some(addr.port())
        );
        let token = url.path().strip_prefix("/unlock/").expect("unlock path");
        assert!(token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()));
        url
    };
    let first = link();
    let unlock_path = fixture.0.join("state/operator-unlock");
    assert_eq!(
        fs::metadata(&unlock_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!fixture.0.join("state/operator-session").exists());
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
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .resolve("localhost", addr)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    // Never include the one-use URL or cookie contents in test diagnostics.
    let redeem = |url| {
        client
            .get(url)
            .send()
            .unwrap_or_else(|_| panic!("unlock request failed"))
    };
    let response = redeem(first.clone());
    assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);
    let cookie = response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(
        cookie.starts_with("control_operator=")
            && cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
    );
    assert!(!unlock_path.exists());
    assert_eq!(
        redeem(first.clone()).status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let second = link();
    assert!(
        first != second,
        "each invocation must generate a fresh link"
    );
    assert_eq!(redeem(second).status(), reqwest::StatusCode::SEE_OTHER);
}

#[test]
fn project_access_requires_operator_confirmation_and_survives_restart() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let fixture = Fixture(std::env::temp_dir().join(format!(
        "control-project-http-{}-{stamp}",
        std::process::id()
    )));
    fs::create_dir(&fixture.0).unwrap();
    let source = fixture.0.join("project");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("code.rs"), "fn main() {}\n").unwrap();
    fs::write(source.join(".env"), "synthetic fixture, not a credential").unwrap();
    assert!(fixture
        .command()
        .args(["config", "init"])
        .status()
        .unwrap()
        .success());
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = reservation.local_addr().unwrap();
    drop(reservation);
    let start_server = || {
        let mut server = Server(
            fixture
                .command()
                .arg("serve")
                .env("CONTROL_ADDR", addr.to_string())
                .spawn()
                .unwrap(),
        );
        let begin = Instant::now();
        while !healthy(addr) {
            assert!(server.0.try_wait().unwrap().is_none());
            assert!(begin.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(25));
        }
        server
    };
    let mut server = start_server();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .resolve("localhost", addr)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
    let origin = format!("http://localhost:{}", addr.port());
    let link = fixture
        .command()
        .args(["open", "--link"])
        .env("CONTROL_ADDR", addr.to_string())
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(link.status.success());
    let url = String::from_utf8(link.stdout).unwrap();
    let response = client
        .get(url.trim())
        .send()
        .unwrap_or_else(|_| panic!("unlock request failed"));
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let created = client
        .post(format!("{origin}/chats"))
        .header("Origin", &origin)
        .header("Cookie", &cookie)
        .form(&[("lens", "none")])
        .send()
        .unwrap();
    assert_eq!(created.status(), 303);
    let chat = created
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let project_url = format!("{origin}{chat}/project");
    assert_eq!(
        client
            .post(&project_url)
            .header("Origin", &origin)
            .form(&[("path", source.to_str().unwrap())])
            .send()
            .unwrap()
            .status(),
        401
    );
    let request = client
        .post(&project_url)
        .header("Origin", &origin)
        .header("Cookie", &cookie)
        .form(&[("path", source.to_str().unwrap())])
        .send()
        .unwrap();
    assert_eq!(request.status(), 303);
    assert!(
        !fixture.0.join("state/projects").exists(),
        "Intent must not import host files"
    );
    let page = client
        .get(&project_url)
        .header("Cookie", &cookie)
        .send()
        .unwrap()
        .text()
        .unwrap();
    let nonce = page
        .split("name='request' value='")
        .nth(1)
        .unwrap()
        .split('\'')
        .next()
        .unwrap();
    let approve = |request: &str| {
        client
            .post(&project_url)
            .header("Origin", &origin)
            .header("Cookie", &cookie)
            .form(&[("approve", "yes"), ("request", request)])
            .send()
            .unwrap()
    };
    assert_eq!(approve("stale-request").status(), 400);
    assert!(!fixture.0.join("state/projects").exists());
    assert_eq!(approve(nonce).status(), 303);
    assert_eq!(approve(nonce).status(), 400, "Approval cannot be replayed");
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.0.join("state/state.json")).unwrap()).unwrap();
    let grant = saved["chats"][0]["project"]["grant"].as_str().unwrap();
    let seed = fixture
        .0
        .join("state/projects/grants")
        .join(grant)
        .join("seed");
    assert_eq!(fs::read(seed.join("code.rs")).unwrap(), b"fn main() {}\n");
    assert!(!seed.join(".env").exists());
    assert!(!source.join(".git").exists());
    assert_eq!(
        unsafe { libc::kill(server.0.id() as i32, libc::SIGTERM) },
        0
    );
    assert!(server.0.wait().unwrap().success());
    server = start_server();
    let page = client
        .get(&project_url)
        .header("Cookie", &cookie)
        .send()
        .unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.text().unwrap().contains("Project · isolated worktree"));
    assert_eq!(
        unsafe { libc::kill(server.0.id() as i32, libc::SIGTERM) },
        0
    );
    assert!(server.0.wait().unwrap().success());
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
        .args([
            "config",
            "init",
            "--provider",
            "claude",
            "--source-root",
            env!("CARGO_MANIFEST_DIR")
        ])
        .status()
        .unwrap()
        .success());
    // Setup's sole Claude selection must survive both reruns and server startup.
    let model_path = fixture.0.join("config/model.json");
    let original = fs::read(&model_path).unwrap();
    assert!(fixture
        .command()
        .args(["config", "init", "--provider", "codex"])
        .status()
        .unwrap()
        .success());
    assert_eq!(fs::read(&model_path).unwrap(), original);
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
        // Check the running UI, rather than state.json: provider choices are private
        // configuration and intentionally omitted from shared runtime snapshots.
        let output = fixture
            .command()
            .args(["open", "--link"])
            .env("CONTROL_ADDR", addr.to_string())
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        assert!(output.status.success());
        let link = String::from_utf8(output.stdout).unwrap();
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .resolve("localhost", addr)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .get(link.trim())
            .send()
            .unwrap_or_else(|_| panic!("sign-in failed"));
        assert_eq!(response.status(), reqwest::StatusCode::SEE_OTHER);
        let cookie = response.headers()[reqwest::header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let page = client
            .get(format!("http://localhost:{}/?view=providers", addr.port()))
            .header(reqwest::header::COOKIE, cookie)
            .send()
            .unwrap()
            .text()
            .unwrap();
        let rows = page
            .split("<section class='model-row'>")
            .collect::<Vec<_>>();
        assert_eq!(
            rows.len(),
            2,
            "the running UI should list exactly one configured model"
        );
        let selected = rows[1].split("</section>").next().unwrap();
        assert!(
            selected.contains("aria-pressed='true'")
                && selected.contains("value='claude'")
                && selected.contains("value='sonnet'")
        );
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
