//! Credential-free preview services with loopback-only ingress and no egress.
//! Each preview has a separate browser hostname; Control cookies are host-only.
use crate::runtime::RunStatus;
use crate::{State, usage::Attempt};
use control_isolation::{
    Access, Exec, ExecSpec, Isolation, Limits, Logs, MountKind, Network, Observed, Pid1, RunSpec, Size, Stream,
    Streams, Target, Tmpfs, User,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::Path,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    sync::{Arc, Mutex},
    time::Duration,
};
crate::application::status_text::status_enum! {
    /// A preview's lifecycle, saved as its lowercase name.
    pub enum PreviewStatus {
        Preparing = "preparing",
        Running = "running",
        Failed = "failed",
        Stopped = "stopped",
        Unavailable = "unavailable",
    }
}
impl PreviewStatus {
    /// Not confirmed stopped: the preview keeps its slot and stays listed.
    pub fn is_retained(&self) -> bool {
        match self {
            Self::Stopped => false,
            Self::Preparing | Self::Running | Self::Failed | Self::Unavailable | Self::Unrecognised(_) => true,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Preview {
    pub id: String,
    pub chat: String,
    pub attempt: Attempt,
    pub port: u16,
    pub command_hash: String,
    pub container_id: Option<String>,
    pub url: Option<String>,
    pub status: PreviewStatus,
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
        json!({
            "name": "control_preview_start",
            "description": concat!(
                "Start one local preview for this approved project attempt in a credential-free Nix container. Bind ",
                "your server to 0.0.0.0 at the given container port. The project is read-only; write build/cache ",
                "files under /tmp (CARGO_TARGET_DIR=/tmp/target). No external network. Returns a separate-origin ",
                "localhost URL. Service persists after the turn until stopped."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "minLength": 1, "maxLength": 16000},
                    "port": {"type": "integer", "minimum": 1, "maximum": 65535}
                },
                "required": ["command", "port"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false}
        }),
        json!({
            "name": "control_preview_stop",
            "description": "Stop this attempt's owned preview. Does not stop the agent or another chat's service.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false}
        }),
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
fn launch(isolation: &dyn Isolation, preview: &Preview, command: &str, workspace: &Path) -> io::Result<String> {
    if unsafe { libc::getuid() } == 0 {
        return Err(fail("Run Control as a non-root operator to start previews"));
    }
    let source = workspace
        .to_str()
        .filter(|s| workspace.is_absolute() && !s.contains([',', '\r', '\n']))
        .ok_or_else(|| fail("Invalid preview workspace"))?;
    let node = preview.attempt.node_id.as_deref().ok_or_else(|| fail("Missing node"))?;
    let image = preview
        .attempt
        .image
        .as_deref()
        .ok_or_else(|| fail("Missing pinned image"))?;
    let result = (|| {
        let owner = User {
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        };
        let id = isolation.run_detached(&container(&preview.id, node, image, owner, source, command))?;
        verify(isolation, &preview.id, &preview.attempt)?;
        Ok(id.as_str().to_owned())
    })();
    if result.is_err() {
        let _ = stop_resource(isolation, preview);
    }
    result
}
/// The detached preview container serving `command` from a read-only workspace.
fn container(name: &str, node: &str, image: &str, owner: User, source: &str, command: &str) -> RunSpec {
    let wrapper = format!(
        "set -eu\nexec 2>&1\ncat > /tmp/control-port-forward.rs <<'CONTROL_FORWARD_SOURCE'\n{}\nCONTROL_FORWARD_SOURCE\nrustc --edition=2024 /tmp/control-port-forward.rs -o /tmp/control-port-forward\nexec bash -c \"$1\"",
        include_str!("../tools/port-forward/main.rs")
    );
    RunSpec {
        name: name.into(),
        image: image.into(),
        argv: ["bash", "-c", &wrapper, "control-preview", command]
            .map(str::to_owned)
            .to_vec(),
        user: Some(owner),
        workdir: Some("/workspace".into()),
        limits: Limits {
            pids: Some(128),
            memory: Some(Size::Gib(1)),
            cpus: Some(2),
            ..Limits::default()
        },
        network: Network::None,
        logs: Logs::Local {
            max_size: Size::Mib(1),
            max_files: 2,
        },
        pid1: Pid1::Init,
        ..RunSpec::default()
    }
    .label("dev.control.node", node)
    .label("dev.control.attempt", name)
    .label("dev.control.purpose", "preview")
    .tmpfs(Tmpfs {
        exec: Exec::Allow,
        nodev: true,
        ..Tmpfs::new("/tmp", Size::Mib(512))
    })
    .tmpfs(Tmpfs::new("/home/agent", Size::Mib(64)).owned(owner))
    .env("HOME", "/home/agent")
    .env("CARGO_TARGET_DIR", "/tmp/target")
    .bind(source, "/workspace", Access::ReadOnly)
}
pub fn verify(isolation: &dyn Isolation, name: &str, attempt: &Attempt) -> io::Result<()> {
    if attempt.project.is_none() || self::name(attempt)? != name {
        return Err(fail("Preview has no approved project attempt"));
    }
    let id = attempt.workspace_id.as_deref().unwrap();
    let workspace = crate::runtime::workspace(id).map_err(fail)?.canonicalize()?;
    let [observed] = <[Observed; 1]>::try_from(isolation.inspect(&[Target::Name(name)])?)
        .map_err(|_| fail("Invalid preview inspection"))?;
    let mounted = observed
        .mounts
        .iter()
        .any(|m| m.destination == "/workspace" && Some(m.source.as_str()) == workspace.to_str() && !m.writable);
    let no_extra_binds = observed.mounts.iter().filter(|m| m.kind == MountKind::Bind).count() == 1;
    let offline = observed.network == "none"
        && observed
            .ports
            .as_ref()
            .is_some_and(|ports| ports.values().all(|published| !published));
    if observed.name != format!("/{name}")
        || Some(observed.image_ref.as_str()) != attempt.image.as_deref()
        || observed.label("dev.control.node") != attempt.node_id.as_deref()
        || observed.label("dev.control.attempt") != Some(name)
        || !observed.read_only_root
    {
        return Err(fail("Preview ownership does not match its approved attempt"));
    }
    if !mounted || !no_extra_binds {
        return Err(fail("Preview mount policy does not match the approved workspace"));
    }
    if !offline {
        return Err(fail(
            "Preview runtime did not disable networking and native port publishing",
        ));
    }
    Ok(())
}
pub fn stop_resource(isolation: &dyn Isolation, preview: &Preview) -> io::Result<()> {
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
        isolation,
        &preview.id,
        preview.attempt.node_id.as_deref().ok_or_else(|| fail("Missing node"))?,
        preview.attempt.image.as_deref().ok_or_else(|| fail("Missing image"))?,
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
    let hash = hash.as_ref().iter().map(|b| format!("{b:02x}")).collect::<String>();
    let listener;
    let mut preview = {
        let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
        if crate::shutdown::requested() {
            return Err(fail("Control is stopping; no preview was started"));
        }
        let run = state
            .runtime
            .runs
            .get(run_id)
            .filter(|r| r.status == RunStatus::Running)
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
            if existing.command_hash == hash && existing.port == input.port && existing.status == PreviewStatus::Running
            {
                return Ok(json!({"id":id,"url":existing.url}));
            }
            if existing.status.is_retained() {
                return Err(fail("Stop this attempt's existing preview before changing its service"));
            }
        }
        if state.previews.iter().filter(|p| p.status.is_retained()).count() >= 16 {
            return Err(fail("Stop an existing preview before starting another (limit 16)"));
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
            status: PreviewStatus::Preparing,
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
    let result = launch(crate::engine::isolation(), &preview, &input.command, &workspace);
    preview.container_id = result.as_ref().ok().cloned();
    let mut state = match shared.lock() {
        Ok(state) => state,
        Err(error) => {
            drop(error);
            fence_launch(shared, &preview);
            return Err(fail("State unavailable; inspect retained preview intent"));
        }
    };
    let checkpoint = checkpoint_launch(
        &mut state,
        run_id,
        &preview,
        result.is_ok(),
        crate::shutdown::requested(),
        crate::storage::save,
    );
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
    let active = state.runtime.runs.get(run_id).is_some_and(|r| {
        r.status == RunStatus::Running
            && r.attempts
                .last()
                .is_some_and(|a| a.workspace_id == preview.attempt.workspace_id)
    });
    let record = state
        .previews
        .iter_mut()
        .find(|p| p.id == preview.id)
        .ok_or_else(|| fail("Preview intent is missing"))?;
    if record.status != PreviewStatus::Preparing || !Arc::ptr_eq(&record.stopped, &preview.stopped) {
        return Err(fail("Preview was stopped or replaced while starting"));
    }
    record.container_id.clone_from(&preview.container_id);
    let running = launched && active && !stopping;
    record.status = if running {
        PreviewStatus::Running
    } else {
        PreviewStatus::Failed
    };
    if !running {
        record.stopped.store(true, Ordering::Relaxed);
    }
    for chat in state.chats.iter_mut().filter(|c| c.id == preview.chat) {
        chat.revision = chat.revision.saturating_add(1);
    }
    if save(state).is_err() || !running {
        let record = state.previews.iter_mut().find(|p| p.id == preview.id).unwrap();
        record.status = PreviewStatus::Failed;
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
    let status = if preview.container_id.is_some() && stop_resource(crate::engine::isolation(), preview).is_ok() {
        PreviewStatus::Stopped
    } else {
        PreviewStatus::Failed
    };
    if let Ok(mut state) = shared.lock()
        && let Some(record) = state
            .previews
            .iter_mut()
            .find(|p| p.id == preview.id && Arc::ptr_eq(&p.stopped, &preview.stopped))
    {
        record.status = status;
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
        stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return Ok(());
    }
    stream.set_read_timeout(Some(Duration::from_secs(300)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = stream.try_clone()?;
    let id = preview
        .container_id
        .as_deref()
        .ok_or_else(|| fail("Preview container is not captured"))?;
    let forwarder = ExecSpec {
        container: id.into(),
        input: control_isolation::Input::Attached,
        argv: vec!["/tmp/control-port-forward".into(), preview.port.to_string()],
        ..ExecSpec::default()
    };
    let mut child =
        crate::engine::isolation().exec(&forwarder, Streams::new(Stream::Piped, Stream::Piped, Stream::Null))?;
    let mut input = child.stdin.take().ok_or_else(|| fail("Missing proxy input"))?;
    input.write_all(&header)?;
    let mut output = child.stdout.take().ok_or_else(|| fail("Missing proxy output"))?;
    let child = Arc::new(Mutex::new(child));
    let writing = child.clone();
    let writer = std::thread::spawn(move || {
        let result = io::copy(&mut reader, &mut input);
        drop(input);
        if result.is_err()
            && let Ok(mut child) = writing.lock()
        {
            let _ = child.kill();
        }
    });
    let _ = io::copy(&mut output, &mut stream);
    let _ = stream.shutdown(Shutdown::Both);
    let _ = writer.join();
    let mut child = child.lock().map_err(|_| fail("Proxy process unavailable"))?;
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}
fn allowed_header(header: &[u8], url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(url) else {
        return false;
    };
    let expected = format!("{}:{}", url.host_str().unwrap_or(""), url.port().unwrap_or(80));
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
            while !crate::shutdown::requested() && !preview.stopped.load(Ordering::Relaxed) {
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
    let records = shared.lock().map(|s| s.previews.clone()).unwrap_or_default();
    for preview in records.into_iter().filter(|p| p.status == PreviewStatus::Running) {
        if crate::shutdown::requested() {
            break;
        }
        let result = (|| {
            let isolation = crate::engine::isolation();
            verify(isolation, &preview.id, &preview.attempt)?;
            let observed = isolation.inspect(&[Target::Name(&preview.id)])?;
            if observed.first().map(|o| o.id.as_str()) != preview.container_id.as_deref() {
                return Err(fail("Preview container identity changed"));
            }
            let url =
                reqwest::Url::parse(preview.url.as_deref().unwrap_or("")).map_err(|_| fail("Invalid preview URL"))?;
            if url.host_str() != Some(format!("{}.localhost", preview.id).as_str()) {
                return Err(fail("Invalid preview origin"));
            }
            let listener = TcpListener::bind(("127.0.0.1", url.port().ok_or_else(|| fail("Missing preview port"))?))?;
            proxy(shared.clone(), preview.clone(), listener)
        })();
        if result.is_err()
            && let Ok(mut state) = shared.lock()
        {
            if let Some(record) = state.previews.iter_mut().find(|p| {
                p.id == preview.id && p.status == PreviewStatus::Running && p.container_id == preview.container_id
            }) {
                record.status = PreviewStatus::Unavailable;
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
    stop_resource(crate::engine::isolation(), &preview)?;
    let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let record = state
        .previews
        .iter_mut()
        .find(|p| p.id == id)
        .filter(|p| p.container_id == preview.container_id && p.url == preview.url)
        .ok_or_else(|| fail("Preview changed while stopping; review its current state"))?;
    record.status = PreviewStatus::Stopped;
    record.stopped.store(true, Ordering::Relaxed);
    for c in state.chats.iter_mut().filter(|c| c.id == chat) {
        c.revision = c.revision.saturating_add(1);
    }
    crate::storage::save(&state)
}
pub fn links(previews: &[Preview], chat: &str) -> String {
    let mut html = String::new();
    for preview in previews.iter().filter(|p| p.chat == chat && p.status.is_retained()) {
        let link = preview
            .url
            .as_ref()
            .filter(|_| preview.status == PreviewStatus::Running)
            .map(|url| {
                format!(
                    "<a target='_blank' rel='noopener noreferrer' href='{}'>Open preview ↗</a>",
                    crate::escape(url)
                )
            })
            .unwrap_or_else(|| format!("<span>Preview · {}</span>", crate::escape(preview.status.as_str())));
        html.push_str(&format!("<div class='preview-link'>{link}<form method='post' target='_top' action='/dm/{chat}/preview/stop'><input type='hidden' name='id' value='{}'><button>Stop</button></form></div>",crate::escape(&preview.id)));
    }
    html
}

#[cfg(test)]
mod tests;
