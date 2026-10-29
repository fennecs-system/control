//! Local host tools: read-only Hive access, web search, and bounded offline execution.
use crate::harness::Hive;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
};
const READ_TOOLS: &[&str] = &[
    "control.chats.list",
    "control.chat.read",
    "control.lenses.list",
    "control.lens.read",
    "control.lenses.resolve",
    "control.rooms.list",
    "control.room.read",
];
fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn image() -> io::Result<String> {
    let image = std::env::var("CONTROL_BROWSER_IMAGE")
        .ok()
        .or_else(|| fs::read_to_string(".control/browser-image-id").ok())
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !crate::browser::valid_image(&image) {
        return Err(fail("No pinned browser image. Build the Nix browser-image and configure .control/browser-image-id."));
    }
    Ok(image)
}
pub(crate) fn source(root: &Path, input: &str) -> io::Result<PathBuf> {
    let candidate = root.join(input);
    let resolved = fs::canonicalize(&candidate)?;
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
    Ok(candidate)
}
pub(crate) fn render_preview(
    input: &Path,
    width: u16,
    height: u16,
) -> io::Result<crate::browser::Capture> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let directory = Path::new(".control/browser");
    fs::create_dir_all(directory)?;
    if fs::symlink_metadata(directory)?.file_type().is_symlink() {
        return Err(fail("Artifact directory must not be a symlink"));
    }
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    #[cfg(target_os = "macos")]
    let flags = 0x100 | 0x4;
    #[cfg(not(target_os = "macos"))]
    let flags = 0x20000 | 0x800;
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
    for entry in fs::read_dir(directory)? {
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
    crate::browser::capture(&image()?, input, width, height)
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
        *connection = Some(Hive::connect("http://127.0.0.1:8080")?);
    }
    Ok(connection.as_mut().unwrap())
}
fn dispatch(connection: &mut Option<Hive>, root: &Path, request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let result = (|| -> io::Result<Value> {
        if request["jsonrpc"] != "2.0" {
            return Err(fail("Invalid JSON-RPC request"));
        }
        match request["method"].as_str().unwrap_or("") {
            "initialize" => Ok(
                json!({"protocolVersion":if request["params"]["protocolVersion"]=="2025-03-26" {"2025-03-26"}else{"2025-11-25"},"capabilities":{"tools":{}},"serverInfo":{"name":"control-local-tools","version":env!("CARGO_PKG_VERSION")}}),
            ),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let mut tools = match hive(connection).and_then(|h| h.rpc("tools/list", json!({})))
                {
                    Ok(value) => value["tools"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|t| READ_TOOLS.contains(&t["name"].as_str().unwrap_or("")))
                        .map(|mut t| {
                            t["annotations"] = json!({"readOnlyHint":true,"openWorldHint":false});
                            t
                        })
                        .collect::<Vec<_>>(),
                    Err(_) => Vec::new(),
                };
                tools.push(json!({"name":"control.browser.capture","description":"Render a self-contained HTML preview inside the configured root using offline Nix Firefox. Returns a PNG artifact. No network or host browser profile. Requires the pinned browser image.","inputSchema":{"type":"object","properties":{"input":{"type":"string"},"width":{"type":"integer","minimum":240,"maximum":2560},"height":{"type":"integer","minimum":240,"maximum":2560}},"required":["input"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}}));
                tools.extend(crate::toolbox::definitions());
                Ok(json!({"tools":tools}))
            }
            "tools/call" => {
                let name = request["params"]["name"].as_str().unwrap_or("");
                let arguments = &request["params"]["arguments"];
                let result = if name == "control.browser.capture" {
                    capture(root, arguments)
                } else if crate::toolbox::known(name) {
                    Ok(crate::toolbox::result(crate::toolbox::call(
                        name, arguments,
                    )))
                } else if READ_TOOLS.contains(&name) {
                    hive(connection).and_then(|h| h.rpc("tools/call", request["params"].clone()))
                } else {
                    Err(fail("Tool unavailable: this bridge exposes read-only Control access, web search, offline captures and Rust snippets"))
                };
                Ok(result.unwrap_or_else(
                    |e| json!({"content":[{"type":"text","text":e.to_string()}],"isError":true}),
                ))
            }
            _ => Err(fail("Method not found")),
        }
    })();
    match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(e) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":e.to_string()}}),
    }
}
pub fn run(args: &[String]) -> io::Result<()> {
    if args.len() != 2 || args[0] != "--root" {
        return Err(fail("control mcp --root /absolute/control/directory"));
    }
    let root = fs::canonicalize(&args[1])?;
    std::env::set_current_dir(&root)?;
    // Codex launches this native host application; worker containers never receive its credential.
    let mut connection = None;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let mut bytes = Vec::new();
        let count = input.by_ref().take(65537).read_until(b'\n', &mut bytes)?;
        if count == 0 {
            return Ok(());
        }
        if bytes.len() > 65536 {
            return Err(fail("MCP input exceeds 64 KiB"));
        }
        let request: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => {
                writeln!(
                    output,
                    "{}",
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Invalid JSON"}})
                )?;
                output.flush()?;
                continue;
            }
        };
        if request.get("id").is_none() {
            continue;
        }
        writeln!(output, "{}", dispatch(&mut connection, &root, &request))?;
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
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires the local pinned Nix browser image and generated HTML fixture; no model calls"]
    fn live_browser_capture_returns_an_image_artifact() {
        let root = fs::canonicalize(".").unwrap();
        let result = dispatch(
            &mut None,
            &root,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control.browser.capture","arguments":{"input":".control/browser-preview/chat.html","width":390,"height":844}}}),
        );
        assert_eq!(result["result"]["isError"], false, "Browser MCP failed");
        let data = result["result"]["content"][1]["data"].as_str().unwrap();
        let png = STANDARD.decode(data).unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 390);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 844);
        let path = result["result"]["structuredContent"]["path"]
            .as_str()
            .unwrap();
        assert_eq!(fs::read(path).unwrap(), png);
        assert_eq!(result["result"]["structuredContent"]["width"], 390);
        assert_eq!(result["result"]["structuredContent"]["height"], 844);
    }
    #[test]
    fn bridge_rejects_state_changes_without_contacting_hive() {
        let result = dispatch(
            &mut None,
            Path::new("."),
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"control.chat.send","arguments":{"id":"anything","message":"/improve change source"}}}),
        );
        assert_eq!(result["result"]["isError"], true);
        assert!(source(Path::new("/private/tmp"), "/etc/hosts").is_err());
    }
}
