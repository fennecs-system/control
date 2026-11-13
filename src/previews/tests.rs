
use super::*;
#[test]
fn launch_fences_never_leave_a_running_record_or_link_for_later_saves() {
    for (active, stopping, save_ok, launched) in [
        (true, false, true, true),
        (false, false, true, true),
        (true, true, true, true),
        (true, false, false, true),
        (true, false, true, false),
    ] {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "Preview", "test").unwrap();
        let run = &mut state.runtime.runs[0];
        run.status = if active { "running" } else { "cancelling" }.into();
        let attempt = Attempt {
            workspace_id: Some("generation".into()),
            ..Default::default()
        };
        run.attempts.push(attempt.clone());
        let run_id = run.id.clone();
        let preview = Preview {
            id: "preview".into(),
            chat: chat.clone(),
            attempt,
            port: 8080,
            command_hash: "hash".into(),
            container_id: Some("a".repeat(64)),
            url: Some("http://preview.localhost:1234/".into()),
            status: "preparing".into(),
            stopped: Arc::default(),
        };
        state.previews.push(preview.clone());
        let result = checkpoint_launch(&mut state, &run_id, &preview, launched, stopping, |_| {
            if save_ok {
                Ok(())
            } else {
                Err(fail("disk full"))
            }
        });
        let running = active && !stopping && save_ok && launched;
        assert_eq!(result.is_ok(), running);
        assert_eq!(state.previews[0].status == "running", running);
        assert_eq!(links(&state, &chat).contains("Open preview"), running);
        assert_eq!(preview.stopped.load(Ordering::Relaxed), !running);
        // An unrelated later save cannot revive a fenced service.
        let saved: State = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(saved.previews[0].status == "running", running);
        assert_eq!(saved.previews[0].container_id, preview.container_id);
    }
}
#[test]
fn preview_origin_gate_rejects_control_hosts_cookies_and_ambiguous_headers() {
    let url = "http://fixture.localhost:43210/";
    assert!(allowed_header(
        b"GET / HTTP/1.1\r\nHost: fixture.localhost:43210\r\nCookie: app_session=fixture\r\n\r\n",
        url
    ));
    for header in [
        "GET / HTTP/1.1\r\nHost: localhost:43210\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: fixture.localhost:43210\r\nCookie: control_operator=synthetic\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: fixture.localhost:43210\r\nHost: localhost\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: fixture.localhost:43210\r\n Cookie: synthetic\r\n\r\n",
    ] {
        assert!(!allowed_header(header.as_bytes(), url));
    }
}
#[test]
fn previews_require_an_active_captured_project_and_reject_forged_mounts() {
    assert!(
        serde_json::from_value::<Input>(json!({"command":"true","port":8080,"mount":"/"})).is_err()
    );
    let state = Arc::new(Mutex::new(State::default()));
    assert!(
        start(
            &state,
            "unknown",
            Input {
                command: "true".into(),
                port: 8080
            }
        )
        .is_err()
    );
    let mut locked = state.lock().unwrap();
    let chat = crate::chat::create(&mut locked);
    crate::chat::send(&mut locked, &chat, "test", "fixture").unwrap();
    let run = &mut locked.runtime.runs[0];
    run.status = "running".into();
    run.attempts.push(Attempt::default());
    let id = run.id.clone();
    drop(locked);
    assert!(
        start(
            &state,
            &id,
            Input {
                command: "true".into(),
                port: 8080
            }
        )
        .unwrap_err()
        .to_string()
        .contains("trust a project")
    );
    assert!(state.lock().unwrap().previews.is_empty());
}
#[test]
#[ignore = "requires isolated CONTROL_STATE_DIR/CONTROL_CONFIG_DIR under /private/tmp/control-preview-qa and CONTROL_QA_IMAGE; no credentials or model calls"]
fn live_preview_is_local_readonly_credential_free_and_survives_run_completion() {
    use std::{
        fs,
        time::{Duration, Instant},
    };
    let state_dir = crate::config::state_directory().unwrap();
    assert!(
        state_dir
            .parent()
            .is_some_and(|p| p.parent() == Some(Path::new("/private/tmp"))
                && p.file_name()
                    .is_some_and(|s| s.to_string_lossy().starts_with("control-preview-qa."))),
        "Use a fresh isolated QA state directory"
    );
    crate::storage::initialize().unwrap();
    let image = std::env::var("CONTROL_QA_IMAGE").expect("Pinned local image digest");
    assert!(image.starts_with("sha256:"));
    let generation = format!("preview-qa-{}", std::process::id());
    let workspace = crate::runtime::workspace(&generation).unwrap();
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("server.rs"),r#"use std::{io::{Read,Write},net::TcpListener};fn main(){for stream in TcpListener::bind("0.0.0.0:8080").unwrap().incoming(){let mut stream=stream.unwrap();let mut buf=[0;4096];let _=stream.read(&mut buf);stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 18\r\nConnection: close\r\n\r\ncontrol-preview-ok").unwrap();}}"#).unwrap();
    let mut state = State::default();
    let chat = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &chat, "preview fixture", "qa").unwrap();
    let run = &mut state.runtime.runs[0];
    run.status = "running".into();
    run.attempts.push(Attempt {
        image: Some(image),
        node_id: Some(crate::config::node_id().unwrap()),
        workspace_id: Some(generation),
        project: Some("fixture-approved".into()),
        ..Default::default()
    });
    let run_id = run.id.clone();
    let state = Arc::new(Mutex::new(state));
    struct Cleanup(Arc<Mutex<State>>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for preview in self.0.lock().unwrap().previews.clone() {
                let _ = stop_resource(&preview);
            }
        }
    }
    let _cleanup = Cleanup(state.clone());
    let command = "set -eu; test ! -e /home/agent/.codex/auth.json; test ! -e /home/agent/.claude/.credentials.json; test ! -S /var/run/docker.sock; test ! -S /nix/var/nix/daemon-socket/socket; for path in /workspace/forbidden-write /bin/forbidden-write /nix/store/forbidden-write; do if touch \"$path\" 2>/dev/null; then exit 42; fi; done; if timeout 1 bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>/dev/null; then exit 43; fi; rustc --edition=2024 /workspace/server.rs -o /tmp/server; exec /tmp/server";
    let result = start(
        &state,
        &run_id,
        Input {
            command: command.into(),
            port: 8080,
        },
    )
    .unwrap();
    let url = reqwest::Url::parse(result["url"].as_str().unwrap()).unwrap();
    assert!(url.host_str().unwrap().ends_with(".localhost"));
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .resolve(
            url.host_str().unwrap(),
            ([127, 0, 0, 1], url.port().unwrap()).into(),
        )
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let begin = Instant::now();
    loop {
        if client
            .get(url.clone())
            .send()
            .and_then(|r| r.text())
            .is_ok_and(|text| text == "control-preview-ok")
        {
            break;
        }
        if begin.elapsed() >= Duration::from_secs(20) {
            let name = result["id"].as_str().unwrap();
            let logs = query(&["logs", "--tail", "40", name]).unwrap_or_default();
            let guest = query(&[
                "exec",
                name,
                "bash",
                "-c",
                "printf 'GET / HTTP/1.1\\r\\nHost: fixture\\r\\nConnection: close\\r\\n\\r\\n' | /tmp/control-port-forward 8080",
            ]);
            let response = client
                .get(url.clone())
                .send()
                .map(|r| (r.status(), r.text()));
            panic!(
                "Synthetic preview never became ready: logs={logs}; guest={guest:?}; response={response:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!workspace.join("forbidden-write").exists());
    {
        // A proxy must serve independently of a busy application state lock.
        let _locked = state.lock().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            client.get(url.clone()).send().unwrap().text().unwrap(),
            "control-preview-ok"
        );
    }
    assert_eq!(
        client
            .get(url.clone())
            .header("Cookie", "control_operator=synthetic")
            .send()
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(format!("http://127.0.0.1:{}/", url.port().unwrap()))
            .send()
            .unwrap()
            .status(),
        403
    );
    {
        let mut locked = state.lock().unwrap();
        locked.runtime.runs[0].status = "completed".into();
    }
    crate::lifecycle::reconcile(
        &crate::lifecycle::owners(&state.lock().unwrap().runtime.runs),
        &[],
    )
    .unwrap();
    assert_eq!(
        client.get(url.clone()).send().unwrap().text().unwrap(),
        "control-preview-ok"
    );
    let saved = serde_json::to_vec(&*state.lock().unwrap()).unwrap();
    let restarted: State = serde_json::from_slice(&saved).unwrap();
    assert_eq!(restarted.previews[0].url.as_deref(), Some(url.as_str()));
    // Simulate the old process releasing its listener, then restore only
    // durable intent and the verified existing container into a fresh node.
    state.lock().unwrap().previews[0].status = "restoring".into();
    state.lock().unwrap().previews[0]
        .stopped
        .store(true, Ordering::Relaxed);
    let start = Instant::now();
    loop {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", url.port().unwrap())) {
            drop(listener);
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(25));
    }
    let state = Arc::new(Mutex::new(restarted));
    let _restored_cleanup = Cleanup(state.clone());
    restore(state.clone());
    assert_eq!(
        client.get(url.clone()).send().unwrap().text().unwrap(),
        "control-preview-ok"
    );
    let id = result["id"].as_str().unwrap();
    assert!(stop(&state, "another-chat", id).is_err());
    let mut wrong_identity = state.lock().unwrap().previews[0].clone();
    wrong_identity.container_id = Some("0".repeat(64));
    assert!(
        stop_resource(&wrong_identity).is_err(),
        "A replacement identity must never be stopped"
    );
    stop(&state, &chat, id).unwrap();
    stop(&state, &chat, id).unwrap();
    assert_eq!(state.lock().unwrap().previews[0].status, "stopped");
    state.lock().unwrap().runtime.runs[0].status = "running".into();
    let replacement = super::start(
        &state,
        &run_id,
        Input {
            command: command.into(),
            port: 8080,
        },
    )
    .unwrap();
    assert_eq!(replacement["id"], result["id"]);
    assert_ne!(
        state.lock().unwrap().previews[0].container_id,
        wrong_identity.container_id
    );
    stop(&state, &chat, id).unwrap();
}
