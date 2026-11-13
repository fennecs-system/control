
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};
struct Server {
    child: Child,
}
impl Server {
    fn stop(&mut self) {
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGTERM);
        };
        assert!(self.child.wait().unwrap().success());
    }
    fn crash(&mut self) {
        self.child.kill().unwrap();
        let _ = self.child.wait();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn wait(mut condition: impl FnMut() -> bool, seconds: u64, label: &str) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(seconds),
            "Timed out: {label}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}
fn assert_closed(result: io::Result<usize>) {
    assert!(
        matches!(result, Ok(0))
            || result.is_err_and(|e| matches!(
                e.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
            )),
        "Connection did not close"
    );
}
struct Echo {
    address: SocketAddr,
    accepted: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}
impl Echo {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let a = accepted.clone();
        let b = closed.clone();
        let c = cancel.clone();
        let handle = thread::spawn(move || {
            let mut clients = Vec::new();
            while !c.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        let a = a.clone();
                        let b = b.clone();
                        let c = c.clone();
                        clients.push(thread::spawn(move || {
                            socket
                                .set_read_timeout(Some(Duration::from_millis(200)))
                                .unwrap();
                            let mut data = Vec::new();
                            let mut buffer = [0; 64];
                            let mut counted = false;
                            loop {
                                match socket.read(&mut buffer) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        data.extend_from_slice(&buffer[..n]);
                                        if !counted {
                                            a.fetch_add(1, Ordering::AcqRel);
                                            counted = true;
                                        }
                                    }
                                    Err(e)
                                        if matches!(
                                            e.kind(),
                                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                                        ) =>
                                    {
                                        if c.load(Ordering::Acquire) {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            let _ = socket.write_all(b"host:");
                            let _ = socket.write_all(&data);
                            let _ = socket.shutdown(Shutdown::Write);
                            b.fetch_add(1, Ordering::AcqRel);
                        }));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25))
                    }
                    Err(_) => break,
                }
            }
            for client in clients {
                let _ = client.join();
            }
        });
        Self {
            address,
            accepted,
            closed,
            cancel,
            handle: Some(handle),
        }
    }
}
impl Drop for Echo {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
const PROGRAM: &str = r#"
use std::{io::{Read,Write},net::{TcpListener,TcpStream,Shutdown},thread,time::Duration};
fn main(){
 let echo=TcpListener::bind("127.0.0.1:33001").unwrap();
 thread::spawn(move||for stream in echo.incoming(){let mut s=stream.unwrap();thread::spawn(move||{
   let mut body=Vec::new();let _=s.read_to_end(&mut body);std::fs::write("/workspace/inbound-size",body.len().to_string()).unwrap();
   let _=s.write_all(b"service:");let _=s.write_all(&body);let _=s.shutdown(Shutdown::Write);
 });});
 thread::spawn(||{let listener=TcpListener::bind("127.0.0.1:33003").unwrap();for stream in listener.incoming(){thread::spawn(move||{let mut socket=stream.unwrap();thread::sleep(Duration::from_secs(7));let mut buffer=[0;8192];let(mut count,mut sum)=(0u64,0u64);loop{match socket.read(&mut buffer){Ok(0)=>break,Ok(n)=>{count+=n as u64;sum+=buffer[..n].iter().map(|b|u64::from(*b)).sum::<u64>();},Err(_)=>return}}let _=write!(socket,"received:{count}:{sum}");let _=socket.shutdown(Shutdown::Write);});}});
 thread::spawn(||{let listener=TcpListener::bind("127.0.0.1:33005").unwrap();for stream in listener.incoming(){let mut s=stream.unwrap();thread::spawn(move||{s.write_all(b"banner\n").unwrap();let mut body=Vec::new();let _=s.read_to_end(&mut body);let _=s.write_all(b"after:");let _=s.write_all(&body);let _=s.shutdown(Shutdown::Write);});}});
 let mut first=true;loop{if let Ok(mut s)=TcpStream::connect("127.0.0.1:33002"){if first{s.write_all(b"outbound").unwrap();s.shutdown(Shutdown::Write).unwrap();let mut reply=Vec::new();s.read_to_end(&mut reply).unwrap();if reply==b"host:outbound"{std::fs::write("/workspace/outbound-result",b"half-close-ok").unwrap();first=false;}}else{let _=s.write_all(b"hold");let mut reply=Vec::new();let _=s.read_to_end(&mut reply);}}thread::sleep(Duration::from_millis(100));}
}"#;
#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE Full profile and CONTROL_QA_CONTROL_BIN; isolated service commands, downloads and peer routing, no models"]
fn live_service_commands_download_and_connect_two_private_services() {
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let binary = std::env::var("CONTROL_QA_CONTROL_BIN").unwrap();
    let root = PathBuf::from("/private/tmp").join(format!(
        "control-service-interaction-{}-{}",
        std::process::id(),
        now()
    ));
    let state_dir = root.join("state");
    let config = root.join("config");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir(&config).unwrap();
    let node = ring::digest::digest(&ring::digest::SHA256, root.to_string_lossy().as_bytes())
        .as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    fs::write(state_dir.join("node-id"), &node).unwrap();
    fs::set_permissions(state_dir.join("node-id"), fs::Permissions::from_mode(0o600)).unwrap();
    let cookie = "d".repeat(64);
    fs::write(state_dir.join("operator-session"), &cookie).unwrap();
    fs::set_permissions(
        state_dir.join("operator-session"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let url = format!("http://{address}");
    let (mut state, run, _) = super::tests::fixture();
    let template = serde_json::to_value(&state.services[0]).unwrap();
    state.services.clear();
    let mut ids = Vec::new();
    for name in ["source", "client", "probe"] {
        let mut value = template.clone();
        let id = format!("svc-{name}-{node}");
        ids.push(id.clone());
        value["resource"]["id"] = json!(id);
        value["resource"]["owner"]["node"] = json!(node);
        value["resource"]["image"] = json!(image);
        value["resource"]["phase"] = json!(Phase::Requested);
        value["resource"]["lease"] = serde_json::to_value(if name == "probe" {
            control_core::resources::Lease::new(now(), 180).unwrap()
        } else {
            control_core::resources::Lease::until_stopped(now()).unwrap()
        })
        .unwrap();
        value["workspace_exec"] = json!(true);
        value["profile"] = json!(crate::worker_profile::Profile::inspect(&image).unwrap());
        value["command"] = json!(if name == "probe" {
            "printf 'short probe stdout\\n'; printf 'short probe stderr\\n' >&2; exit 7"
        } else {
            "exec sleep 180"
        });
        state.services.push(serde_json::from_value(value).unwrap());
    }
    // Mirror a delegated worker creating its own client while holding a
    // grant to the parent's server, rather than testing only one owner.
    let client_chat = crate::chat::create(&mut state);
    state.services[1].resource.owner.chat = client_chat.clone().try_into().unwrap();
    state.runtime.runs[0].chat_id = Some(client_chat);
    state.runtime.runs[0].resource_grants =
        vec![state.services[0].grant(), state.services[2].grant()];
    state.runtime.runs[0].status = "completed".into();
    let write = |state: &State| {
        fs::write(
            state_dir.join("state.json"),
            serde_json::to_vec(state).unwrap(),
        )
        .unwrap()
    };
    let read = || {
        serde_json::from_slice::<State>(&fs::read(state_dir.join("state.json")).unwrap()).unwrap()
    };
    write(&state);
    let launch = || {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("server.log"))
            .unwrap();
        Server {
            child: Command::new(&binary)
                .arg("serve")
                .env("CONTROL_STATE_DIR", &state_dir)
                .env("CONTROL_CONFIG_DIR", &config)
                .env("CONTROL_CODEX_AUTH_FILE", root.join("missing-qa-auth.json"))
                .env(
                    "CONTROL_CLAUDE_AUTH_FILE",
                    root.join("missing-qa-claude.json"),
                )
                .env("CONTROL_ADDR", address.to_string())
                .env_remove("CONTROL_WORKER_IMAGE")
                .env_remove("CONTROL_CLAUDE_IMAGE")
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        }
    };
    struct Cleanup {
        node: String,
        root: PathBuf,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Ok(bytes) = fs::read(self.root.join("state/state.json"))
                && let Ok(state) = serde_json::from_slice::<State>(&bytes)
            {
                for service in &state.services {
                    if let Some(id) = service.resource.phase.container()
                        && crate::services::port_binding(service, id).is_ok()
                    {
                        let _ = crate::engine::cli()
                            .args(["rm", "-f", id.as_str()])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                    }
                }
            }
            println!(
                "Service interaction fixture retained at {} (node {})",
                self.root.display(),
                self.node
            );
        }
    }
    let _cleanup = Cleanup {
        node: node.clone(),
        root: root.clone(),
    };
    let mut server = launch();
    wait(
        || {
            let state = read();
            matches!(state.services[0].resource.phase, Phase::Running(_))
                && matches!(state.services[1].resource.phase, Phase::Running(_))
                && matches!(state.services[2].resource.phase, Phase::Stopped(_))
        },
        25,
        "two services and retained short probe",
    );
    let probe = serde_json::to_value(&read().services[2]).unwrap();
    assert!(
        probe["output"]
            .as_str()
            .unwrap()
            .contains("short probe stdout")
    );
    assert!(
        probe["output"]
            .as_str()
            .unwrap()
            .contains("short probe stderr")
    );
    assert_eq!(probe["exit_code"], 7);
    server.stop();
    state = read();
    state.runtime.runs[0].status = "running".into();
    let source_command = r#"set -eu
cat > /workspace/server.rs <<'RUST'
use std::{io::{Read,Write},net::{TcpListener,Shutdown}};
fn main(){for socket in TcpListener::bind("127.0.0.1:34001").unwrap().incoming(){std::thread::spawn(move||{let mut socket=socket.unwrap();let mut data=Vec::new();socket.read_to_end(&mut data).unwrap();socket.write_all(b"peer:").unwrap();socket.write_all(&data).unwrap();socket.shutdown(Shutdown::Write).unwrap();});}}
RUST
rustc --edition=2024 /workspace/server.rs -o /workspace/server
nohup /workspace/server > /workspace/server.log 2>&1 < /dev/null &
printf source-ready
"#;
    let source_receipt = crate::services::enqueue_test_command(
        &mut state,
        &run,
        "source-setup",
        &ids[0],
        source_command,
        30,
    )
    .unwrap();
    let staged=crate::services::enqueue_test_command(&mut state,&run,"client-files",&ids[1],"set -eu; printf persistent > /workspace/settings; if timeout 1 bash -c 'exec 3<>/dev/tcp/127.0.0.1/34003' 2>/dev/null; then exit 42; fi; printf staged-without-network",10).unwrap();
    let ingress = admit(
        &mut state,
        &run,
        "attempt-a",
        "source-ingress",
        Input {
            service_id: ids[0].clone().try_into().unwrap(),
            name: "source-ingress".to_owned().try_into().unwrap(),
            direction: Direction::Ingress,
            service_port: NonZeroU16::new(34001).unwrap(),
            target: None,
            peer_port_id: None,
        },
        node.clone().try_into().unwrap(),
        |_| Ok(()),
    )
    .unwrap();
    let ingress_id = ingress["port_id"].as_str().unwrap().to_owned();
    state.runtime.runs[0].status = "completed".into();
    write(&state);
    server = launch();
    let execution = |service: usize, id: &str| {
        let state = read();
        let value = serde_json::to_value(&state.services[service]).unwrap();
        value["executions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .unwrap()
            .clone()
    };
    wait(
        || {
            execution(0, source_receipt["execution_id"].as_str().unwrap())["status"] == "completed"
                && execution(1, staged["execution_id"].as_str().unwrap())["status"] == "completed"
        },
        35,
        "service file creation after model turn",
    );
    assert_eq!(
        execution(1, staged["execution_id"].as_str().unwrap())["exit_code"],
        0
    );
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let post = |id: &str, fields: &str| {
        let response = client
            .post(format!("{url}/tools/ports"))
            .header("Cookie", format!("control_operator={cookie}"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!("id={id}&{fields}"))
            .send()
            .unwrap();
        assert_eq!(response.status(), 200);
    };
    let port = |id: &str| {
        read()
            .ports
            .into_iter()
            .find(|p| p.id.as_str() == id)
            .unwrap()
    };
    assert_eq!(port(&ingress_id).status, Status::Pending);
    post(&ingress_id, "seconds=-1");
    wait(
        || {
            port(&ingress_id)
                .approval
                .as_ref()
                .is_some_and(|a| a.host_port != 0)
        },
        8,
        "source ingress approval",
    );
    let published = port(&ingress_id).approval.unwrap().host_port;
    // A private host HTTP fixture represents an explicitly approved download
    // origin. Its binary body never passes through model text or host mounts.
    let download = TcpListener::bind("127.0.0.1:0").unwrap();
    let download_address = download.local_addr().unwrap();
    let body = (0..262144).map(|n| (n % 251) as u8).collect::<Vec<_>>();
    let expected = ring::digest::digest(&ring::digest::SHA256, &body)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let download_thread = thread::spawn(move || {
        let (mut socket, _) = download.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut request = [0; 4096];
        let _ = socket.read(&mut request).unwrap();
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        socket.write_all(&body).unwrap();
    });
    server.stop();
    state = read();
    state.runtime.runs[0].status = "running".into();
    let mut pending = Vec::new();
    for (name, local, target) in [
        ("peer-route", 34003, None),
        ("asset-download", 34002, Some(download_address)),
    ] {
        let value = admit(
            &mut state,
            &run,
            "attempt-a",
            name,
            Input {
                service_id: ids[1].clone().try_into().unwrap(),
                name: name.to_owned().try_into().unwrap(),
                direction: Direction::Egress,
                service_port: NonZeroU16::new(local).unwrap(),
                target: target.map(|t: SocketAddr| Endpoint {
                    host: "127.0.0.1".into(),
                    port: NonZeroU16::new(t.port()).unwrap(),
                }),
                peer_port_id: target
                    .is_none()
                    .then(|| ingress_id.clone().try_into().unwrap()),
            },
            node.clone().try_into().unwrap(),
            |_| Ok(()),
        )
        .unwrap();
        pending.push(value["port_id"].as_str().unwrap().to_owned());
    }
    state.runtime.runs[0].status = "completed".into();
    write(&state);
    server = launch();
    wait(
        || {
            client
                .get(format!("{url}/health"))
                .send()
                .is_ok_and(|r| r.status().is_success())
        },
        5,
        "approval UI online",
    );
    for id in &pending {
        assert_eq!(port(id).status, Status::Pending);
        post(id, "seconds=-1&loopback=yes");
    }
    assert_eq!(
        port(&pending[0]).approval.unwrap().pinned,
        Some(SocketAddr::from(([127, 0, 0, 1], published)))
    );
    server.stop();
    state = read();
    state.runtime.runs[0].status = "running".into();
    let command = format!(
        r#"set -eu
test "$(cat /workspace/settings)" = persistent
curl --noproxy '*' --fail --silent --show-error --retry 8 --retry-connrefused --retry-delay 1 --max-time 15 --connect-to 'download.invalid:80:127.0.0.1:34002' http://download.invalid/asset -o /workspace/asset
test "$(sha256sum /workspace/asset | cut -d ' ' -f 1)" = {expected}
cat > /workspace/client.rs <<'RUST'
use std::{{io::{{Read,Write}},net::{{TcpStream,Shutdown}},time::{{Duration,Instant}}}};
fn main(){{let rounds=Instant::now();let barrier=std::sync::Arc::new(std::sync::Barrier::new(4));let tasks=(0..4).map(|_|{{let barrier=barrier.clone();std::thread::spawn(move||{{let start=Instant::now();let mut socket=loop{{match TcpStream::connect("127.0.0.1:34003"){{Ok(s)=>break s,Err(_)if start.elapsed()<Duration::from_secs(8)=>std::thread::sleep(Duration::from_millis(100)),Err(e)=>panic!("{{e}}")}}}};socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();barrier.wait();socket.write_all(b"connected").unwrap();socket.shutdown(Shutdown::Write).unwrap();let mut reply=String::new();socket.read_to_string(&mut reply).unwrap();assert_eq!(reply,"peer:connected");}})}}).collect::<Vec<_>>();for task in tasks{{task.join().unwrap();}}println!("peer-connected; 4 concurrent connections in {{:?}}",rounds.elapsed());}}
RUST
rustc --edition=2024 /workspace/client.rs -o /workspace/client
/workspace/client
printf verified-download >> /workspace/completions
"#
    );
    let receipt = crate::services::enqueue_test_command(
        &mut state,
        &run,
        "download-connect",
        &ids[1],
        &command,
        60,
    )
    .unwrap();
    state.runtime.runs[0].status = "completed".into();
    write(&state);
    server = launch();
    let execution_id = receipt["execution_id"].as_str().unwrap();
    wait(
        || execution(1, execution_id)["status"] == "completed",
        65,
        "binary download and cross-container route",
    );
    let outcome = execution(1, execution_id);
    assert_eq!(outcome["exit_code"], 0, "{outcome}");
    assert!(
        outcome["output"]
            .as_str()
            .unwrap()
            .contains("peer-connected")
    );
    println!("{}", outcome["output"].as_str().unwrap());
    download_thread.join().unwrap();
    server.stop();
    state = read();
    state.runtime.runs[0].status = "running".into();
    let check=crate::services::enqueue_test_command(&mut state,&run,"verify-retention",&ids[1],"test $(wc -c < /workspace/asset) = 262144; test $(cat /workspace/completions) = verified-download; printf retained-once",10).unwrap();
    state.runtime.runs[0].status = "completed".into();
    write(&state);
    server = launch();
    wait(
        || execution(1, check["execution_id"].as_str().unwrap())["status"] == "completed",
        15,
        "private files and exactly one effect across restart",
    );
    assert_eq!(
        execution(1, check["execution_id"].as_str().unwrap())["exit_code"],
        0
    );
    server.stop();
    state = read();
    state.runtime.runs[0].status = "running".into();
    let failed = crate::services::enqueue_test_command(
        &mut state,
        &run,
        "failed-command",
        &ids[1],
        "printf expected-command-error >&2; exit 7",
        10,
    )
    .unwrap();
    state.runtime.runs[0].status = "completed".into();
    write(&state);
    server = launch();
    wait(
        || execution(1, failed["execution_id"].as_str().unwrap())["status"] == "failed",
        15,
        "nonzero command reports failure",
    );
    assert_eq!(
        execution(1, failed["execution_id"].as_str().unwrap())["exit_code"],
        7
    );
    assert!(
        execution(1, failed["execution_id"].as_str().unwrap())["output"]
            .as_str()
            .unwrap()
            .contains("expected-command-error")
    );
    assert!(matches!(
        read().services[1].resource.phase,
        Phase::Running(_)
    ));
    server.stop();
    state = read();
    state.runtime.runs[0].status = "running".into();
    let timeout = crate::services::enqueue_test_command(
        &mut state,
        &run,
        "timeout",
        &ids[1],
        "printf before-timeout; exec sleep 60",
        1,
    )
    .unwrap();
    state.runtime.runs[0].status = "completed".into();
    write(&state);
    server = launch();
    wait(
        || {
            execution(1, timeout["execution_id"].as_str().unwrap())["status"] == "timed_out"
                && matches!(read().services[1].resource.phase, Phase::Stopped(_))
        },
        15,
        "timed-out command stops only its service",
    );
    assert!(matches!(
        read().services[0].resource.phase,
        Phase::Running(_)
    ));
    server.stop();
    state = read();
    state.runtime.runs[0].status = "running".into();
    let crash = crate::services::enqueue_test_command(
        &mut state,
        &run,
        "crash-command",
        &ids[0],
        "printf once >> /workspace/crash-marker; exec sleep 60",
        30,
    )
    .unwrap();
    state.runtime.runs[0].status = "completed".into();
    write(&state);
    server = launch();
    wait(
        || execution(0, crash["execution_id"].as_str().unwrap())["status"] == "running",
        10,
        "command running before host crash",
    );
    let running = read();
    let source = &running.services[0];
    let container = source.resource.phase.container().unwrap();
    crate::services::port_binding(source, container).unwrap();
    wait(
        || {
            crate::engine::query(&["exec", container.as_str(), "cat", "/workspace/crash-marker"])
                .is_ok_and(|text| text == "once")
        },
        5,
        "remote command effect before crash",
    );
    server.crash();
    server = launch();
    wait(
        || {
            execution(0, crash["execution_id"].as_str().unwrap())["status"] == "interrupted"
                && matches!(read().services[0].resource.phase, Phase::Stopped(_))
        },
        15,
        "uncertain command fenced without replay after crash",
    );
    server.stop();
    println!(
        "Verified service file creation, retained failed output, explicit scoped binary download, peer routing, restart persistence and timeout cleanup."
    );
}
#[test]
#[ignore = "requires CONTROL_QA_SERVICE_IMAGE, CONTROL_QA_CONTROL_BIN and Docker; isolated generic TCP fixtures, no models"]
fn live_tcp_ports_preserve_half_close_and_recover_after_host_death() {
    let image = std::env::var("CONTROL_QA_SERVICE_IMAGE").unwrap();
    let binary = std::env::var("CONTROL_QA_CONTROL_BIN").unwrap();
    let echo = Echo::start();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(format!(
        "/private/tmp/control-tcp-live-{}-{stamp}",
        std::process::id()
    ));
    let state_dir = root.join("state");
    let config_dir = root.join("config");
    fs::create_dir_all(&state_dir).unwrap();
    fs::create_dir(&config_dir).unwrap();
    let digest = ring::digest::digest(&ring::digest::SHA256, root.to_string_lossy().as_bytes());
    let node = digest.as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    fs::write(state_dir.join("node-id"), &node).unwrap();
    fs::set_permissions(state_dir.join("node-id"), fs::Permissions::from_mode(0o600)).unwrap();
    // Synthetic credential for this private fixture only; never read personal credentials.
    let cookie = "c".repeat(64);
    fs::write(state_dir.join("operator-session"), &cookie).unwrap();
    fs::set_permissions(
        state_dir.join("operator-session"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let address = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let url = format!("http://{address}");
    let (mut state, run, _) = super::tests::fixture();
    state.runtime.runs[0].status = "completed".into();
    let service = &mut state.services[0];
    service.resource.owner.node = node.clone().try_into().unwrap();
    service.resource.id = format!("svc-{node}").try_into().unwrap();
    service.resource.image = image.clone().try_into().unwrap();
    service.resource.phase = Phase::Requested;
    service.resource.lease = control_core::resources::Lease::new(now(), 180).unwrap();
    let mut value = serde_json::to_value(&*service).unwrap();
    value["profile"] = json!(crate::worker_profile::Profile::inspect(&image).unwrap());
    value["workspace_exec"] = json!(true);
    value["command"] = json!(format!(
        "cat > /tmp/fixture.rs <<'CONTROL_RUST'\n{PROGRAM}\nCONTROL_RUST\nrustc --edition=2024 -O /tmp/fixture.rs -o /workspace/fixture\nexec /workspace/fixture"
    ));
    state.services[0] = serde_json::from_value(value).unwrap();
    let write = |s: &State| {
        fs::write(state_dir.join("state.json"), serde_json::to_vec(s).unwrap()).unwrap()
    };
    let read = || {
        serde_json::from_slice::<State>(&fs::read(state_dir.join("state.json")).unwrap()).unwrap()
    };
    write(&state);
    let launch = || {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("server.log"))
            .unwrap();
        Server {
            child: Command::new(&binary)
                .arg("serve")
                .env("CONTROL_STATE_DIR", &state_dir)
                .env("CONTROL_CONFIG_DIR", &config_dir)
                .env("CONTROL_CODEX_AUTH_FILE", root.join("missing-qa-auth.json"))
                .env(
                    "CONTROL_CLAUDE_AUTH_FILE",
                    root.join("missing-qa-claude.json"),
                )
                .env("CONTROL_ADDR", address.to_string())
                .env_remove("CONTROL_WORKER_IMAGE")
                .env_remove("CONTROL_CLAUDE_IMAGE")
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap(),
        }
    };
    let mut server = launch();
    wait(
        || matches!(read().services[0].resource.phase, Phase::Running(_)),
        25,
        "service running",
    );
    server.stop();
    state = read();
    let service_id = state.services[0].resource.id.clone();
    let container = state.services[0]
        .resource
        .phase
        .container()
        .unwrap()
        .clone();
    crate::services::relay_capable(&state.services[0]).unwrap();
    crate::services::port_binding(&state.services[0], &container).unwrap();
    struct Cleanup(ContainerId);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = crate::engine::cli()
                .args(["rm", "-f", self.0.as_str()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    let _cleanup = Cleanup(container.clone());
    state.runtime.runs[0].status = "running".into();
    for (name, direction, service_port, target) in [
        ("ingress", Direction::Ingress, 33001, None),
        (
            "egress",
            Direction::Egress,
            33002,
            Some(Endpoint {
                host: "127.0.0.1".into(),
                port: NonZeroU16::new(echo.address.port()).unwrap(),
            }),
        ),
        ("expiry", Direction::Ingress, 33001, None),
        ("death", Direction::Ingress, 33001, None),
        ("stall", Direction::Ingress, 33003, None),
        ("banner", Direction::Ingress, 33005, None),
        (
            "idle",
            Direction::Egress,
            33004,
            Some(Endpoint {
                host: "127.0.0.1".into(),
                port: NonZeroU16::new(echo.address.port()).unwrap(),
            }),
        ),
    ] {
        admit(
            &mut state,
            &run,
            "attempt-a",
            name,
            Input {
                service_id: service_id.clone(),
                name: name.to_owned().try_into().unwrap(),
                direction,
                service_port: NonZeroU16::new(service_port).unwrap(),
                target,
                peer_port_id: None,
            },
            node.clone().try_into().unwrap(),
            |_| Ok(()),
        )
        .unwrap();
    }
    for i in 0..2 {
        let id = state.ports[i].id.as_str().to_owned();
        approve(
            &mut state,
            &id,
            100,
            i == 1,
            if i == 1 { Some(echo.address) } else { None },
            now(),
            |_| Ok(()),
        )
        .unwrap();
    }
    state.runtime.runs[0].status = "completed".into();
    write(&state);
    server = launch();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let post = |id: &str, fields: &str| {
        let response = client
            .post(format!("{url}/tools/ports"))
            .header("Cookie", format!("control_operator={cookie}"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!("id={id}&{fields}"))
            .send()
            .unwrap();
        assert_eq!(response.status(), 200, "port operator action failed");
    };
    let named = |name: &str| read().ports.into_iter().find(|p| p.name.0 == name).unwrap();
    wait(
        || {
            client
                .get(format!("{url}/health"))
                .send()
                .is_ok_and(|r| r.status().is_success())
        },
        5,
        "HTTP server ready",
    );
    let ui_ready = |name: &str| {
        client
            .get(format!("{url}/tools/ports"))
            .header("Cookie", format!("control_operator={cookie}"))
            .send()
            .ok()
            .and_then(|r| r.text().ok())
            .is_some_and(|html| {
                let document = scraper::Html::parse_document(&html);
                document
                    .select(&scraper::Selector::parse("article").unwrap())
                    .any(|a| {
                        a.select(&scraper::Selector::parse("h3").unwrap())
                            .any(|h| h.text().collect::<String>() == name)
                            && a.text().collect::<String>().contains(" · ready")
                    })
            })
    };
    post(named("idle").id.as_str(), "seconds=100&loopback=yes");
    wait(
        || ui_ready("idle"),
        5,
        "idle egress helper listening before crash",
    );
    wait(
        || named("ingress").approval.as_ref().unwrap().host_port != 0,
        8,
        "ingress bind receipt",
    );
    let published = named("ingress").approval.unwrap().host_port;
    let roundtrip = |port: u16| {
        let mut socket = TcpStream::connect(("127.0.0.1", port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket.write_all(b"hello").unwrap();
        socket.shutdown(Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        socket.read_to_end(&mut reply).unwrap();
        assert_eq!(reply, b"service:hello");
    };
    roundtrip(published);
    wait(
        || echo.accepted.load(Ordering::Acquire) >= 2,
        8,
        "outbound half-close and held session",
    );
    assert_eq!(
        crate::engine::query(&[
            "exec",
            container.as_str(),
            "cat",
            "/workspace/outbound-result"
        ])
        .unwrap(),
        "half-close-ok"
    );
    let before = echo.accepted.load(Ordering::Acquire);
    server.crash();
    let restart = Instant::now();
    server = launch();
    wait(
        || echo.accepted.load(Ordering::Acquire) > before,
        10,
        "outbound listener released and reattached after host death",
    );
    wait(
        || ui_ready("idle"),
        8,
        "idle helper releases port after abrupt host death",
    );
    post(named("idle").id.as_str(), "revoke=yes");
    assert_eq!(named("ingress").approval.unwrap().host_port, published);
    roundtrip(published);
    println!(
        "Abrupt host restart and egress rebind: {:?}",
        restart.elapsed()
    );
    post(named("banner").id.as_str(), "seconds=30");
    wait(
        || {
            named("banner")
                .approval
                .as_ref()
                .is_some_and(|a| a.host_port != 0)
        },
        3,
        "server-first banner bind",
    );
    let mut banner =
        TcpStream::connect(("127.0.0.1", named("banner").approval.unwrap().host_port)).unwrap();
    banner
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let mut greeting = [0; 7];
    banner.read_exact(&mut greeting).unwrap();
    assert_eq!(&greeting, b"banner\n");
    banner.write_all(b"thanks").unwrap();
    banner.shutdown(Shutdown::Write).unwrap();
    let mut reply = Vec::new();
    banner.read_to_end(&mut reply).unwrap();
    assert_eq!(reply, b"after:thanks");
    post(named("banner").id.as_str(), "revoke=yes");
    for (prefix, remainder) in [
        (
            b"F".as_slice(),
            format!(
                "OO / HTTP/1.1\r\nHost: localhost\r\nCookie: control_operator={cookie}\r\n\r\n"
            )
            .into_bytes(),
        ),
        (
            b"POST / HTTP/1.1\r\n".as_slice(),
            format!("Cookie: control_operator={cookie}\r\nContent-Length: 1\r\n\r\n\0")
                .into_bytes(),
        ),
        (&[0x16], vec![3, 1, 0, 4, 1, 0, 0, 0]),
    ] {
        let mut browser = TcpStream::connect(("127.0.0.1", published)).unwrap();
        browser.set_nodelay(true).unwrap();
        browser
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        browser.write_all(prefix).unwrap();
        thread::sleep(Duration::from_millis(25));
        let _ = browser.write_all(&remainder);
        let mut bytes = [0; 32];
        assert_closed(browser.read(&mut bytes));
        wait(
            || {
                crate::engine::query(&[
                    "exec",
                    container.as_str(),
                    "cat",
                    "/workspace/inbound-size",
                ])
                .is_ok_and(|n| n == "0")
            },
            3,
            "browser bytes rejected before service forwarding",
        );
    }
    let mut held = TcpStream::connect(("127.0.0.1", published)).unwrap();
    held.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    held.write_all(b"held").unwrap();
    let revoke = Instant::now();
    post(named("ingress").id.as_str(), "revoke=yes");
    let mut buffer = [0; 64];
    assert_closed(held.read(&mut buffer));
    wait(
        || named("ingress").status == Status::Stopped,
        5,
        "ingress stop acknowledgement",
    );
    assert!(TcpStream::connect(("127.0.0.1", published)).is_err());
    println!("Ingress revocation closed socket: {:?}", revoke.elapsed());
    let closed = echo.closed.load(Ordering::Acquire);
    post(named("egress").id.as_str(), "revoke=yes");
    wait(
        || echo.closed.load(Ordering::Acquire) > closed,
        5,
        "egress remote socket closed on revoke",
    );
    post(named("expiry").id.as_str(), "seconds=2");
    wait(
        || {
            named("expiry")
                .approval
                .as_ref()
                .is_some_and(|a| a.host_port != 0)
        },
        2,
        "expiry listener bind",
    );
    let expiry_port = named("expiry").approval.unwrap().host_port;
    let mut expiring = TcpStream::connect(("127.0.0.1", expiry_port)).unwrap();
    expiring
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    expiring.write_all(b"wait").unwrap();
    assert_closed(expiring.read(&mut buffer));
    wait(
        || named("expiry").status == Status::Stopped,
        4,
        "expiry acknowledgement",
    );
    post(named("stall").id.as_str(), "seconds=30");
    wait(
        || {
            named("stall")
                .approval
                .as_ref()
                .is_some_and(|a| a.host_port != 0)
        },
        3,
        "stall listener bind",
    );
    let stalled =
        TcpStream::connect(("127.0.0.1", named("stall").approval.unwrap().host_port)).unwrap();
    stalled
        .set_read_timeout(Some(Duration::from_secs(25)))
        .unwrap();
    let mut sending = stalled.try_clone().unwrap();
    sending
        .set_write_timeout(Some(Duration::from_secs(25)))
        .unwrap();
    let writer = thread::spawn(move || {
        let data: [u8; 65536] = std::array::from_fn(|i| i as u8);
        for _ in 0..256 {
            sending.write_all(&data)?;
        }
        sending.shutdown(Shutdown::Write)
    });
    let mut stalled = stalled;
    let stall = Instant::now();
    let mut response = String::new();
    stalled.read_to_string(&mut response).unwrap();
    writer.join().unwrap().unwrap();
    assert_eq!(
        response,
        format!(
            "received:{}:{}",
            65536u64 * 256,
            (0..65536).map(|i| u64::from(i as u8)).sum::<u64>() * 256
        )
    );
    assert!(stall.elapsed() > Duration::from_secs(5));
    println!(
        "Healthy slow reader resumed with exact payload after {:?}",
        stall.elapsed()
    );
    let mut held =
        TcpStream::connect(("127.0.0.1", named("stall").approval.unwrap().host_port)).unwrap();
    held.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut sending = held.try_clone().unwrap();
    sending
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let blocked = thread::spawn(move || {
        let data = [0; 65536];
        for _ in 0..256 {
            if sending.write_all(&data).is_err() {
                break;
            }
        }
    });
    thread::sleep(Duration::from_secs(1));
    let revoked = Instant::now();
    post(named("stall").id.as_str(), "revoke=yes");
    assert_closed(held.read(&mut buffer));
    let _ = blocked.join();
    assert!(revoked.elapsed() < Duration::from_secs(5));
    println!(
        "Revocation closed a backpressured connection after {:?}",
        revoked.elapsed()
    );
    post(named("death").id.as_str(), "seconds=30");
    wait(
        || {
            named("death")
                .approval
                .as_ref()
                .is_some_and(|a| a.host_port != 0)
        },
        3,
        "death listener bind",
    );
    let mut held =
        TcpStream::connect(("127.0.0.1", named("death").approval.unwrap().host_port)).unwrap();
    held.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    held.write_all(b"waiting").unwrap();
    let current = read();
    crate::services::port_binding(&current.services[0], &container).unwrap();
    assert!(
        crate::engine::cli()
            .args(["kill", container.as_str()])
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    assert_closed(held.read(&mut buffer));
    wait(
        || named("death").status == Status::Stopped,
        6,
        "service death revokes its port",
    );
    server.stop();
    println!("TCP fixture retained at {}", root.display());
}
