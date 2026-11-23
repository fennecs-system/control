//! Local host tools: read-only Hive access, web search, and bounded offline execution.
use control_client::Hive;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
};
fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn image() -> io::Result<String> {
    let image = std::env::var("CONTROL_BROWSER_IMAGE")
        .ok()
        .or_else(|| {
            fs::read_to_string(crate::config::directory().ok()?.join("browser-image-id")).ok()
        })
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !crate::browser::valid_image(&image) {
        return Err(fail("No pinned browser image. Run `nix run .#setup -- --browser` to build it and record browser-image-id in the node configuration directory."));
    }
    Ok(image)
}
pub(crate) fn source(root: &Path, input: &str) -> io::Result<PathBuf> {
    let resolved = fs::canonicalize(root.join(input))?;
    if !resolved.starts_with(root)
        || !matches!(
            resolved.extension().and_then(|s| s.to_str()),
            Some("html" | "htm")
        )
    {
        return Err(fail(
            "Select an HTML preview inside the configured tool root",
        ));
    }
    Ok(resolved)
}
pub(crate) fn render_preview(
    input: &Path,
    width: u16,
    height: u16,
) -> io::Result<crate::browser::Capture> {
    render_preview_with_timeout(input, width, height, std::time::Duration::from_secs(60))
}
pub(crate) fn render_preview_with_timeout(
    input: &Path,
    width: u16,
    height: u16,
    timeout: std::time::Duration,
) -> io::Result<crate::browser::Capture> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let directory = crate::config::state("browser").map_err(io::Error::other)?;
    fs::create_dir_all(&directory)?;
    if fs::symlink_metadata(&directory)?.file_type().is_symlink() {
        return Err(fail("Artifact directory must not be a symlink"));
    }
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let flags = libc::O_NOFOLLOW | libc::O_NONBLOCK;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(flags)
        .open(directory.join("lock"))?;
    if !lock.metadata()?.is_file() {
        return Err(fail("Invalid browser lock file"));
    }
    lock.try_lock().map_err(|_| fail("Browser tool is busy"))?;
    let mut count = 0;
    let mut bytes = 0u64;
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        if entry.file_name() == "lock" {
            continue;
        }
        let metadata = entry.path().symlink_metadata()?;
        if !metadata.is_file() {
            return Err(fail("Unexpected browser artifact entry"));
        }
        count += 1;
        bytes = bytes.saturating_add(metadata.len());
    }
    if count >= 64 || bytes > 248 * 1024 * 1024 {
        return Err(fail(
            "Browser artifact quota reached. Remove old captures before retrying.",
        ));
    }
    let image = image()?;
    if timeout == std::time::Duration::from_secs(60) {
        crate::browser::capture(&image, input, width, height, &directory)
    } else {
        crate::browser::capture_with_timeout(&image, input, width, height, &directory, timeout)
    }
}
fn capture(root: &Path, args: &Value) -> io::Result<Value> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        input: String,
        width: Option<u16>,
        height: Option<u16>,
    }
    let args: Input = serde_json::from_value(args.clone())
        .map_err(|_| fail("Expected input HTML path and optional width/height"))?;
    let input = source(root, &args.input)?;
    let result = render_preview(
        &input,
        args.width.unwrap_or(1280),
        args.height.unwrap_or(800),
    )?;
    let bytes = fs::read(&result.path)?;
    Ok(
        json!({"content":[{"type":"text","text":format!("Screenshot: {} ({}×{})",result.path.display(),result.width,result.height)},{"type":"image","mimeType":"image/png","data":STANDARD.encode(bytes)}],"structuredContent":{"path":result.path,"bytes":result.bytes,"width":result.width,"height":result.height},"isError":false}),
    )
}
fn hive(connection: &mut Option<Hive>) -> io::Result<&mut Hive> {
    if connection.is_none() {
        *connection = Some(Hive::connect(&format!("http://{}", crate::config::server_addr()))?);
    }
    Ok(connection.as_mut().unwrap())
}
pub(crate) fn local_definitions() -> Vec<Value> {crate::tool_registry::local_definitions()}
const BRIDGE: control_mcp::Server = control_mcp::Server {
    name: "control-local-tools",
    version: env!("CARGO_PKG_VERSION"),
    instructions: None,
    // This bridge has always answered a non-2.0 request with -32601.
    invalid: (control_mcp::METHOD_NOT_FOUND, "Invalid JSON-RPC request"),
    unknown: (control_mcp::METHOD_NOT_FOUND, "Method not found"),
};
/// Read-only Control tools through the operator connection, plus local ones.
struct Bridge<'a> {
    connection: &'a mut Option<Hive>,
    root: &'a Path,
}
impl control_mcp::Tools for Bridge<'_> {
    fn list(&mut self) -> Vec<Value> {
        let mut tools = match hive(self.connection).and_then(|h| h.rpc("tools/list", json!({}))) {
            Ok(value) => value["tools"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|t| crate::tool_registry::bridge_read(t["name"].as_str().unwrap_or("")))
                .map(|mut t| {
                    t["annotations"] = json!({"readOnlyHint":true,"openWorldHint":false});
                    t
                })
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        tools.extend(local_definitions());
        tools
    }
    fn call(&mut self, call: control_mcp::Call<'_>) -> Value {
        let (name, arguments) = (call.name, call.arguments);
        let result = if name == "control.browser.capture" {
            capture(self.root, arguments)
        } else if crate::toolbox::known(name) {
            Ok(crate::toolbox::result(crate::toolbox::call(name, arguments)))
        } else if crate::tool_registry::bridge_forward(name) {
            hive(self.connection).and_then(|h| h.rpc("tools/call", call.params.clone()))
        } else {
            Err(fail("Tool unavailable: this bridge exposes read-only Control access, web search, offline captures, Rust snippets and solvers"))
        };
        result.unwrap_or_else(|e| control_mcp::failure(&e.to_string()))
    }
}
fn dispatch(connection: &mut Option<Hive>, root: &Path, request: &Value) -> Value {
    control_mcp::serve(&BRIDGE, request, &mut Bridge { connection, root })
}
pub fn run(args: &[String]) -> io::Result<()> {
    let root = match args {
        [] => crate::config::state_directory().map_err(io::Error::other)?,
        [flag, directory] if flag == "--root" && Path::new(directory).is_absolute() => {
            fs::canonicalize(directory)?
        }
        [flag] if flag == "--chat" => return crate::mcp_chat::run(),
        _ => return Err(fail("control mcp [--root /absolute/preview/directory | --chat]")),
    };
    // Codex launches this native host application; worker containers never receive its credential.
    let mut connection = None;
    serve(|request| dispatch(&mut connection, &root, request))
}
/// Newline-delimited JSON-RPC over stdio, shared by every `control mcp` mode:
/// bounded lines, parse errors answered, notifications ignored.
pub(crate) fn serve(mut dispatch: impl FnMut(&Value) -> Value) -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut bytes = Vec::new();
        let count = input.by_ref().take(65537).read_until(b'\n', &mut bytes)?;
        if count == 0 {
            return Ok(());
        }
        let oversized = bytes.len() > 65536;
        if oversized && bytes.last() != Some(&b'\n') {
            // Discard the remainder of the oversized line, then keep serving.
            loop {
                let buffer = input.fill_buf()?;
                if buffer.is_empty() {
                    break;
                }
                let (used, done) = buffer.iter().position(|b| *b == b'\n').map_or((buffer.len(), false), |i| (i + 1, true));
                input.consume(used);
                if done {
                    break;
                }
            }
        }
        let request = match (!oversized).then(|| control_mcp::parse(&bytes)).flatten() {
            Some(v) => v,
            None => {
                let (code, message) = if oversized { (control_mcp::INVALID_REQUEST, "MCP input exceeds 64 KiB") } else { (control_mcp::PARSE_ERROR, "Invalid JSON") };
                writeln!(output, "{}", control_mcp::error(&Value::Null, code, message))?;
                output.flush()?;
                continue;
            }
        };
        if control_mcp::is_notification(&request) {
            continue;
        }
        writeln!(output, "{}", dispatch(&request))?;
        output.flush()?;
    }
}
pub fn browser_cli(args: &[String]) -> io::Result<()> {
    if !(args.len() == 2 || args.len() == 4) || args[0] != "capture" {
        return Err(fail("control browser capture PREVIEW.html [WIDTH HEIGHT]"));
    }
    let width = args
        .get(2)
        .map(|s| s.parse::<u16>())
        .transpose()
        .map_err(io::Error::other)?
        .unwrap_or(1280);
    let height = args
        .get(3)
        .map(|s| s.parse::<u16>())
        .transpose()
        .map_err(io::Error::other)?
        .unwrap_or(800);
    let root = fs::canonicalize(".")?;
    let input = source(&root, &args[1])?;
    let artifact = render_preview(&input, width, height)?;
    println!(
        "{}",
        json!({"path":artifact.path,"width":artifact.width,"height":artifact.height,"bytes":artifact.bytes})
    );
    Ok(())
}
#[cfg(test)]
mod tests;
