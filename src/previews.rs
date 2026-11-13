//! Credential-free preview services with loopback-only ingress and no egress.
//! Each preview has a separate browser hostname; Control cookies are host-only.
use crate::{usage::Attempt, State};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::Path,
    process::Stdio,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    sync::{Arc, Mutex},
    time::Duration,
};
#[derive(Clone, Serialize, Deserialize)]
pub struct Preview {
    pub id: String,
    pub chat: String,
    pub attempt: Attempt,
    pub port: u16,
    pub command_hash: String,
    pub container_id: Option<String>,
    pub url: Option<String>,
    pub status: String,
    #[serde(skip)]
    stopped: Arc<AtomicBool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub command: String,
    pub port: u16,
}
pub fn definitions() -> Vec<Value> {
    vec![
        json!({"name":"control_preview_start","description":"Start one local preview for this approved project attempt in a credential-free Nix container. Bind your server to 0.0.0.0 at the given container port. The project is read-only; write build/cache files under /tmp (CARGO_TARGET_DIR=/tmp/target). No external network. Returns a separate-origin localhost URL. Service persists after the turn until stopped.","inputSchema":{"type":"object","properties":{"command":{"type":"string","minLength":1,"maxLength":16000},"port":{"type":"integer","minimum":1,"maximum":65535}},"required":["command","port"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}),
        json!({"name":"control_preview_stop","description":"Stop this attempt's owned preview. Does not stop the agent or another chat's service.","inputSchema":{"type":"object","properties":{},"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}),
    ]
}
fn fail(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}
fn name(attempt: &Attempt) -> io::Result<String> {
    Ok(format!(
        "control-{}-preview",
        attempt
            .workspace_id
            .as_deref()
            .filter(|s| crate::chat::valid_id(s))
            .ok_or_else(|| fail("Missing workspace identity"))?
    ))
}
fn query(args: &[&str]) -> io::Result<String> {
    crate::engine::query(args)
}
fn launch(preview: &Preview, command: &str, workspace: &Path) -> io::Result<String> {
    if unsafe { libc::getuid() } == 0 {
        return Err(fail("Run Control as a non-root operator to start previews"));
    }
    let source = workspace
        .to_str()
        .filter(|s| workspace.is_absolute() && !s.contains([',', '\r', '\n']))
        .ok_or_else(|| fail("Invalid preview workspace"))?;
    let node = preview
        .attempt
        .node_id
        .as_deref()
        .ok_or_else(|| fail("Missing node"))?;
    let image = preview
        .attempt
        .image
        .as_deref()
        .ok_or_else(|| fail("Missing pinned image"))?;
    let result = (|| {
        let wrapper=format!("set -eu\nexec 2>&1\ncat > /tmp/control-port-forward.rs <<'CONTROL_FORWARD_SOURCE'\n{}\nCONTROL_FORWARD_SOURCE\nrustc --edition=2024 /tmp/control-port-forward.rs -o /tmp/control-port-forward\nexec bash -c \"$1\"",include_str!("../tools/port-forward/main.rs"));
        let id = query(&[
            "run",
            "--detach",
            "--pull=never",
            "--init",
            "--name",
            &preview.id,
            "--label",
            &format!("dev.control.node={node}"),
            "--label",
            &format!("dev.control.attempt={}", preview.id),
            "--label",
            "dev.control.purpose=preview",
            "--network",
            "none",
            "--user",
            &format!("{}:{}", unsafe { libc::getuid() }, unsafe {
                libc::getgid()
            }),
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--pids-limit=128",
            "--memory=1g",
            "--cpus=2",
            "--log-driver=local",
            "--log-opt=max-size=1m",
            "--log-opt=max-file=2",
            "--tmpfs",
            "/tmp:rw,exec,nosuid,nodev,size=512m",
            "--tmpfs",
            &format!(
                "/home/agent:rw,nosuid,uid={},gid={},size=64m",
                unsafe { libc::getuid() },
                unsafe { libc::getgid() }
            ),
            "--env",
            "HOME=/home/agent",
            "--env",
            "CARGO_TARGET_DIR=/tmp/target",
            "--mount",
            &format!("type=bind,src={source},dst=/workspace,readonly"),
            "--workdir",
            "/workspace",
            image,
            "bash",
            "-c",
            &wrapper,
            "control-preview",
            command,
        ])?;
        let id = id.trim().to_owned();
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(fail("Invalid preview container ID"));
        }
        verify(&preview.id, &preview.attempt)?;
        Ok(id)
    })();
    if result.is_err() {
        let _ = stop_resource(preview);
    }
    result
}
pub fn verify(name: &str, attempt: &Attempt) -> io::Result<()> {
    if attempt.project.is_none() || self::name(attempt)? != name {
        return Err(fail("Preview has no approved project attempt"));
    }
    let id = attempt.workspace_id.as_deref().unwrap();
    let workspace = crate::runtime::workspace(id)
        .map_err(fail)?
        .canonicalize()?;
    let text = query(&[
        "inspect",
        "--format",
        r#"{"id":{{json .Id}},"name":{{json .Name}},"image":{{json .Config.Image}},"node":{{json (index .Config.Labels "dev.control.node")}},"attempt":{{json (index .Config.Labels "dev.control.attempt")}},"root_readonly":{{json .HostConfig.ReadonlyRootfs}},"network":{{json .HostConfig.NetworkMode}},"ports":{{json .NetworkSettings.Ports}},"mounts":{{json .Mounts}}}"#,
        name,
    ])?;
    let value: Value =
        serde_json::from_str(text.trim()).map_err(|_| fail("Invalid preview inspection"))?;
    let mounts = value["mounts"]
        .as_array()
        .ok_or_else(|| fail("Missing preview mounts"))?;
    let mounted = mounts.iter().any(|m| {
        m["Destination"] == "/workspace"
            && m["Source"].as_str() == workspace.to_str()
            && m["RW"] == false
    });
    let no_extra_binds = mounts.iter().filter(|m| m["Type"] == "bind").count() == 1;
    let offline = value["network"] == "none"
        && value["ports"]
            .as_object()
            .is_some_and(|ports| ports.values().all(Value::is_null));
    if value["name"] != format!("/{name}")
        || value["image"].as_str() != attempt.image.as_deref()
        || value["node"].as_str() != attempt.node_id.as_deref()
        || value["attempt"] != name
        || value["root_readonly"] != true
    {
        return Err(fail(
            "Preview ownership does not match its approved attempt",
        ));
    }
    if !mounted || !no_extra_binds {
        return Err(fail(
            "Preview mount policy does not match the approved workspace",
        ));
    }
    if !offline {
        return Err(fail(
            "Preview runtime did not disable networking and native port publishing",
        ));
    }
    Ok(())
}
pub fn stop_resource(preview: &Preview) -> io::Result<()> {
    let workspace = crate::runtime::workspace(
        preview
            .attempt
            .workspace_id
            .as_deref()
            .ok_or_else(|| fail("Missing workspace"))?,
    )
    .map_err(fail)?
    .canonicalize()?;
    crate::lifecycle::stop_resource_id(
        &preview.id,
        preview
            .attempt
            .node_id
            .as_deref()
            .ok_or_else(|| fail("Missing node"))?,
        preview
            .attempt
            .image
            .as_deref()
            .ok_or_else(|| fail("Missing image"))?,
        &workspace,
        preview.container_id.as_deref(),
    )
    .map_err(fail)
}
pub fn start(shared: &Arc<Mutex<State>>, run_id: &str, input: Input) -> io::Result<Value> {
    if input.command.trim().is_empty() || input.command.len() > 16000 || input.port == 0 {
        return Err(fail("Expected a server command and container port 1–65535"));
    }
    let hash = ring::digest::digest(&ring::digest::SHA256, input.command.as_bytes());
    let hash = hash
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let listener;
    let mut preview = {
        let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
        if crate::shutdown::requested() {
            return Err(fail("Control is stopping; no preview was started"));
        }
        let run = state
            .runtime
            .runs
            .iter()
            .find(|r| r.id == run_id && r.status == "running")
            .ok_or_else(|| fail("Run is inactive"))?
            .clone();
        crate::identity::authorize(&run.actor, run.action).map_err(fail)?;
        let attempt = run
            .attempts
            .last()
            .filter(|a| a.project.is_some())
            .ok_or_else(|| fail("Open and trust a project before starting local previews"))?
            .clone();
        let id = name(&attempt)?;
        if let Some(existing) = state.previews.iter().find(|p| p.id == id) {
            if existing.command_hash == hash
                && existing.port == input.port
                && existing.status == "running"
            {
                return Ok(json!({"id":id,"url":existing.url}));
            }
            if existing.status != "stopped" {
                return Err(fail(
                    "Stop this attempt's existing preview before changing its service",
                ));
            }
        }
        if state
            .previews
            .iter()
            .filter(|p| p.status != "stopped")
            .count()
            >= 16
        {
            return Err(fail(
                "Stop an existing preview before starting another (limit 16)",
            ));
        }
        listener = TcpListener::bind("127.0.0.1:0")?;
        let host_port = listener.local_addr()?.port();
        let preview = Preview {
            id: id.clone(),
            chat: run.chat_id.clone().ok_or_else(|| fail("Missing chat"))?,
            attempt,
            port: input.port,
            command_hash: hash,
            container_id: None,
            url: Some(format!("http://{id}.localhost:{host_port}/")),
            status: "preparing".into(),
            stopped: Arc::default(),
        };
        let previous = state
            .previews
            .iter()
            .position(|p| p.id == id)
            .map(|index| state.previews.remove(index));
        state.previews.push(preview.clone());
        if crate::storage::save(&state).is_err() {
            state.previews.pop();
            if let Some(previous) = previous {
                state.previews.push(previous);
            }
            return Err(fail("Could not save preview intent"));
        }
        preview
    };
    let workspace = crate::runtime::workspace(preview.attempt.workspace_id.as_deref().unwrap())
        .map_err(fail)?
        .canonicalize()?;
    let result = launch(&preview, &input.command, &workspace);
    preview.container_id = result.as_ref().ok().cloned();
    let mut state = match shared.lock() {
        Ok(state) => state,
        Err(error) => {
            drop(error);
            fence_launch(shared, &preview);
            return Err(fail("State unavailable; inspect retained preview intent"));
        }
    };
    let checkpoint = checkpoint_launch(&mut state, run_id, &preview, result.is_ok(), crate::shutdown::requested(), crate::storage::save);
    drop(state);
    if let Err(error) = checkpoint {
        fence_launch(shared, &preview);
        return Err(error);
    }
    result?;
    if let Err(error) = proxy(shared.clone(), preview.clone(), listener) {
        fence_launch(shared, &preview);
        return Err(error);
    }
    Ok(json!({"id":preview.id,"url":preview.url}))
}

fn checkpoint_launch(
    state: &mut State,
    run_id: &str,
    preview: &Preview,
    launched: bool,
    stopping: bool,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> io::Result<()> {
    let active = state.runtime.runs.iter().any(|r| {
        r.id == run_id
            && r.status == "running"
            && r.attempts
                .last()
                .is_some_and(|a| a.workspace_id == preview.attempt.workspace_id)
    });
    let record = state
        .previews
        .iter_mut()
        .find(|p| p.id == preview.id)
        .ok_or_else(|| fail("Preview intent is missing"))?;
    if record.status != "preparing" || !Arc::ptr_eq(&record.stopped, &preview.stopped) {
        return Err(fail("Preview was stopped or replaced while starting"));
    }
    record.container_id.clone_from(&preview.container_id);
    let running = launched && active && !stopping;
    record.status = if running { "running" } else { "failed" }.into();
    if !running {
        record.stopped.store(true, Ordering::Relaxed);
    }
    for chat in state.chats.iter_mut().filter(|c| c.id == preview.chat) {
        chat.revision = chat.revision.saturating_add(1);
    }
    if save(state).is_err() || !running {
        let record = state
            .previews
            .iter_mut()
            .find(|p| p.id == preview.id)
            .unwrap();
        record.status = "failed".into();
        record.stopped.store(true, Ordering::Relaxed);
        return Err(fail(
            "Preview launch was fenced; inspect its retained intent before retrying",
        ));
    }
    Ok(())
}

fn fence_launch(shared: &Arc<Mutex<State>>, preview: &Preview) {
    preview.stopped.store(true, Ordering::Relaxed);
    // Only a verified stop frees the resource slot. Failed cleanup retains intent.
    let status = if preview.container_id.is_some() && stop_resource(preview).is_ok() {
        "stopped"
    } else {
        "failed"
    };
    if let Ok(mut state) = shared.lock()
        && let Some(record) = state
            .previews
            .iter_mut()
            .find(|p| p.id == preview.id && Arc::ptr_eq(&p.stopped, &preview.stopped))
        {
            record.status = status.into();
            record.stopped.store(true, Ordering::Relaxed);
            let _ = crate::storage::save(&state);
        }
}
fn forward(mut stream: TcpStream, preview: &Preview) -> io::Result<()> {
    // macOS inherits the listener's nonblocking flag on accepted sockets.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut header = Vec::new();
    let end = loop {
        if let Some(index) = header.windows(4).position(|w| w == b"\r\n\r\n") {
            break index + 4;
        }
        if header.len() >= 32768 {
            return Err(fail("Preview header limit exceeded"));
        }
        let mut bytes = [0; 4096];
        let length = stream.read(&mut bytes)?;
        if length == 0 {
            return Ok(());
        }
        header.extend_from_slice(&bytes[..length]);
    };
    if !allowed_header(&header[..end], preview.url.as_deref().unwrap_or("")) {
        stream.write_all(
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
        return Ok(());
    }
    stream.set_read_timeout(Some(Duration::from_secs(300)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = stream.try_clone()?;
    let id = preview
        .container_id
        .as_deref()
        .ok_or_else(|| fail("Preview container is not captured"))?;
    let mut child = crate::engine::scrubbed()
        .args([
            "exec",
            "--interactive",
            id,
            "/tmp/control-port-forward",
            &preview.port.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| fail("Missing proxy input"))?;
    input.write_all(&header)?;
    let mut output = child
        .stdout
        .take()
        .ok_or_else(|| fail("Missing proxy output"))?;
    let child = Arc::new(Mutex::new(child));
    let writing = child.clone();
    let writer = std::thread::spawn(move || {
        let result = io::copy(&mut reader, &mut input);
        drop(input);
        if result.is_err()
            && let Ok(mut child) = writing.lock() {
                let _ = child.kill();
            }
    });
    let _ = io::copy(&mut output, &mut stream);
    let _ = stream.shutdown(Shutdown::Both);
    let _ = writer.join();
    let mut child = child
        .lock()
        .map_err(|_| fail("Proxy process unavailable"))?;
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}
fn allowed_header(header: &[u8], url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    let expected = format!(
        "{}:{}",
        url.host_str().unwrap_or(""),
        url.port().unwrap_or(80)
    );
    let Ok(header) = std::str::from_utf8(header) else {
        return false;
    };
    let mut hosts = 0;
    for line in header.split("\r\n").skip(1).filter(|s| !s.is_empty()) {
        if line.starts_with([' ', '\t']) {
            return false;
        }
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        if name.eq_ignore_ascii_case("host") {
            hosts += 1;
            if !value.trim().eq_ignore_ascii_case(&expected) {
                return false;
            }
        }
        if name.eq_ignore_ascii_case("cookie")
            && value
                .split(';')
                .filter_map(|c| c.trim().split_once('='))
                .any(|(name, _)| name == "control_operator")
        {
            return false;
        }
    }
    hosts == 1
}
fn proxy(_shared: Arc<Mutex<State>>, preview: Preview, listener: TcpListener) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("control-preview-proxy".into())
        .spawn(move || {
            let connections = Arc::new(AtomicUsize::new(0));
            while !crate::shutdown::requested()
                && !preview.stopped.load(Ordering::Relaxed)
            {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if connections.fetch_add(1, Ordering::Relaxed) >= 16 {
                            connections.fetch_sub(1, Ordering::Relaxed);
                            continue;
                        }
                        let count = connections.clone();
                        let target = preview.clone();
                        std::thread::spawn(move || {
                            let _ = forward(stream, &target);
                            count.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50))
                    }
                    Err(_) => break,
                }
            }
        })?;
    Ok(())
}
pub fn restore(shared: Arc<Mutex<State>>) {
    let records = shared
        .lock()
        .map(|s| s.previews.clone())
        .unwrap_or_default();
    for preview in records.into_iter().filter(|p| p.status == "running") {
        if crate::shutdown::requested() {
            break;
        }
        let result = (|| {
            verify(&preview.id, &preview.attempt)?;
            let id = query(&["inspect", "--format", "{{.Id}}", &preview.id])?;
            if Some(id.trim()) != preview.container_id.as_deref() {
                return Err(fail("Preview container identity changed"));
            }
            let url = reqwest::Url::parse(preview.url.as_deref().unwrap_or(""))
                .map_err(|_| fail("Invalid preview URL"))?;
            if url.host_str() != Some(format!("{}.localhost", preview.id).as_str()) {
                return Err(fail("Invalid preview origin"));
            }
            let listener = TcpListener::bind((
                "127.0.0.1",
                url.port().ok_or_else(|| fail("Missing preview port"))?,
            ))?;
            proxy(shared.clone(), preview.clone(), listener)
        })();
        if result.is_err()
            && let Ok(mut state) = shared.lock() {
                if let Some(record) = state.previews.iter_mut().find(|p| {
                    p.id == preview.id
                        && p.status == "running"
                        && p.container_id == preview.container_id
                }) {
                    record.status = "unavailable".into();
                    record.stopped.store(true, Ordering::Relaxed);
                }
                let _ = crate::storage::save(&state);
            }
    }
}
pub fn stop(shared: &Arc<Mutex<State>>, chat: &str, id: &str) -> io::Result<()> {
    let preview = shared
        .lock()
        .map_err(|_| fail("State unavailable"))?
        .previews
        .iter()
        .find(|p| p.chat == chat && p.id == id)
        .cloned()
        .ok_or_else(|| fail("Preview not found in this chat"))?;
    stop_resource(&preview)?;
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let record = state
        .previews
        .iter_mut()
        .find(|p| p.id == id)
        .filter(|p| p.container_id == preview.container_id && p.url == preview.url)
        .ok_or_else(|| fail("Preview changed while stopping; review its current state"))?;
    record.status = "stopped".into();
    record.stopped.store(true, Ordering::Relaxed);
    for c in state.chats.iter_mut().filter(|c| c.id == chat) {
        c.revision = c.revision.saturating_add(1);
    }
    crate::storage::save(&state)
}
pub fn links(state: &State, chat: &str) -> String {
    let mut html = String::new();
    for preview in state
        .previews
        .iter()
        .filter(|p| p.chat == chat && p.status != "stopped")
    {
        let link = preview
            .url
            .as_ref()
            .filter(|_| preview.status == "running")
            .map(|url| {
                format!(
                    "<a target='_blank' rel='noopener noreferrer' href='{}'>Open preview ↗</a>",
                    crate::escape(url)
                )
            })
            .unwrap_or_else(|| {
                format!("<span>Preview · {}</span>", crate::escape(&preview.status))
            });
        html.push_str(&format!("<div class='preview-link'>{link}<form method='post' target='_top' action='/chat/{chat}/preview/stop'><input type='hidden' name='id' value='{}'><button>Stop</button></form></div>",crate::escape(&preview.id)));
    }
    html
}

#[cfg(test)]
mod tests;
