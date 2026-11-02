//! Nix-built, offline Firefox screenshot tool. No browser profile or host directory is shared.
#[path = "../engine-cli.rs"]
mod engine_cli;
#[path = "../platform-flags.rs"]
mod platform_flags;
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const HTML_LIMIT: usize = 4 * 1024 * 1024;
const IMAGE_LIMIT: usize = 8 * 1024 * 1024;
fn fail(s: &str) -> io::Error {
    io::Error::other(s)
}
pub fn valid_image(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn viewport(width: u16, height: u16) -> io::Result<()> {
    if !(240..=2560).contains(&width) || !(240..=2560).contains(&height) {
        return Err(fail("Viewport must be 240–2560 pixels per side"));
    }
    Ok(())
}
fn read_input(path: &Path) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(platform_flags::SAFE_READ)
        .open(path)?;
    if !f.metadata()?.is_file() || f.metadata()?.len() > HTML_LIMIT as u64 {
        return Err(fail("Expected a regular HTML file of at most 4 MiB"));
    }
    let mut bytes = Vec::new();
    f.take(HTML_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > HTML_LIMIT || std::str::from_utf8(&bytes).is_err() {
        return Err(fail("Expected UTF-8 HTML of at most 4 MiB"));
    }
    Ok(bytes)
}
fn private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}
fn private_file(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    f.sync_all()
}
pub struct Capture {
    pub path: PathBuf,
    pub bytes: usize,
    pub width: u16,
    pub height: u16,
}
pub fn capture(
    image: &str,
    input: &Path,
    width: u16,
    height: u16,
    output: &Path,
) -> io::Result<Capture> {
    capture_with_timeout(image, input, width, height, output, Duration::from_secs(60))
}
pub fn capture_with_timeout(
    image: &str,
    input: &Path,
    width: u16,
    height: u16,
    output: &Path,
    timeout: Duration,
) -> io::Result<Capture> {
    if timeout.is_zero() || timeout > Duration::from_secs(60) {
        return Err(fail("Invalid browser deadline"));
    }
    viewport(width, height)?;
    if !valid_image(image) {
        return Err(fail("Browser requires a pinned sha256 image ID"));
    }
    let html = read_input(input)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let scratch =
        std::env::temp_dir().join(format!("control-browser-{}-{stamp}", std::process::id()));
    private_dir(&scratch)?;
    let result = (|| {
        use std::os::unix::fs::PermissionsExt;
        let bundle = scratch.join("input");
        fs::create_dir(&bundle)?;
        fs::set_permissions(&bundle, fs::Permissions::from_mode(0o755))?;
        private_file(&bundle.join("page.html"), &html)?;
        fs::set_permissions(bundle.join("page.html"), fs::Permissions::from_mode(0o444))?;
        let mount = format!(
            "type=bind,src={},dst=/input,readonly",
            bundle
                .to_str()
                .filter(|s| !s.contains([',', '\n', '\r']))
                .ok_or_else(|| fail("Invalid temporary path"))?
        );
        let name = format!("control-browser-{stamp}");
        let mut child = engine_cli::command()
            .args([
                "run",
                "--rm",
                "--pull=never",
                "--name",
                &name,
                "--label",
                "dev.control.tool=browser",
                "--network=none",
                "--user=65532:65532",
                "--read-only",
                "--cap-drop=ALL",
                "--security-opt=no-new-privileges",
                "--memory=1g",
                "--cpus=2",
                "--pids-limit=512",
                "--shm-size=128m",
                "--tmpfs",
                "/tmp:rw,nosuid,mode=1777,size=256m",
                "--mount",
                &mount,
                image,
                "--inside",
                &width.to_string(),
                &height.to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .by_ref()
                .take(IMAGE_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let deadline = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if deadline.elapsed() > timeout {
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if status.is_none() {
            let _ = engine_cli::command()
                .args(["rm", "-f", &name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = child.kill();
            let _ = child.wait();
        }
        let bytes = reader
            .join()
            .map_err(|_| fail("Browser output reader failed"))??;
        if !status.is_some_and(|s| s.success())
            || bytes.len() > IMAGE_LIMIT
            || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        {
            return Err(fail("Browser capture failed or exceeded its limits"));
        }
        // A private persistent artifact, not a writable mount exposed to Firefox.
        fs::create_dir_all(output)?;
        let artifact = output.join(format!("capture-{stamp}.png"));
        private_file(&artifact, &bytes)?;
        Ok(Capture {
            path: fs::canonicalize(artifact)?,
            bytes: bytes.len(),
            width,
            height,
        })
    })();
    let _ = fs::remove_dir_all(scratch);
    result
}
pub fn inside(width: u16, height: u16) -> io::Result<()> {
    viewport(width, height)?;
    fs::create_dir("/tmp/profile")?;
    let mut child = Command::new("firefox")
        .args([
            "--headless",
            "--profile",
            "/tmp/profile",
            "--screenshot",
            "/tmp/screenshot.png",
            "--window-size",
            &format!("{width},{height}"),
            "file:///input/page.html",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(fail("Firefox failed"));
            }
            break;
        }
        if start.elapsed() > Duration::from_secs(45) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(fail("Firefox timed out"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut png = Vec::new();
    fs::File::open("/tmp/screenshot.png")?
        .take(IMAGE_LIMIT as u64 + 1)
        .read_to_end(&mut png)?;
    if png.len() > IMAGE_LIMIT || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(fail("Invalid screenshot"));
    }
    io::stdout().lock().write_all(&png)
}
#[allow(dead_code)]
fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 || args[1] != "--inside" {
        return Err(fail("Internal browser entrypoint"));
    }
    inside(
        args[2].parse().map_err(io::Error::other)?,
        args[3].parse().map_err(io::Error::other)?,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_authority_and_viewports_fail_before_docker() {
        assert!(capture(
            "control-browser:nix",
            Path::new("missing"),
            1280,
            800,
            Path::new("unused")
        )
        .is_err());
        assert!(capture(
            &format!("sha256:{}", "a".repeat(64)),
            Path::new("missing"),
            1,
            800,
            Path::new("unused")
        )
        .is_err());
        assert!(valid_image(&format!("sha256:{}", "a".repeat(64))));
        assert!(!valid_image("sha256:xyz"));
    }
    #[cfg(unix)]
    #[test]
    fn input_reader_rejects_symlinks_and_nonregular_files() {
        use std::os::unix::fs::symlink;
        let dir =
            std::env::temp_dir().join(format!("control-browser-input-{}", std::process::id()));
        private_dir(&dir).unwrap();
        private_file(&dir.join("page.html"), b"<p>Preview</p>").unwrap();
        symlink(dir.join("page.html"), dir.join("link.html")).unwrap();
        assert!(read_input(&dir.join("link.html")).is_err());
        assert!(read_input(&dir).is_err());
        assert_eq!(
            read_input(&dir.join("page.html")).unwrap(),
            b"<p>Preview</p>"
        );
        let _ = fs::remove_dir_all(dir);
    }
}
