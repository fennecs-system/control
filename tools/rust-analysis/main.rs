//! Standalone, std-only launcher and container entry point.
use std::env;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("Analysis could not run: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--inside") {
        return analyze(args.get(1).map(String::as_str).unwrap_or("fmt"));
    }
    if args.len() != 4 || !matches!(args[3].as_str(), "fmt" | "clippy") {
        return Err("usage: control-analyze IMAGE_SHA256 SOURCE OUTPUT fmt|clippy".into());
    }
    if !valid_image(&args[0]) {
        return Err("image must be pinned as sha256:<64 hex digits>".into());
    }
    let source = fs::canonicalize(&args[1]).map_err(|_| "source directory unavailable")?;
    if !source.join("Cargo.toml").is_file() {
        return Err("source must contain Cargo.toml".into());
    }
    fs::create_dir_all(&args[2]).map_err(|_| "cannot create output directory")?;
    let output = fs::canonicalize(&args[2]).map_err(|_| "output directory unavailable")?;
    if output == source || source.starts_with(&output) || output.starts_with(&source) {
        return Err("output must be separate from the source tree".into());
    }
    if output
        .to_str()
        .is_none_or(|s| s.contains([',', '\n', '\r']))
    {
        return Err("output path contains an unsupported mount separator".into());
    }
    let snapshot = Snapshot::new()?;
    let mut totals = (0_usize, 0_u64);
    export(&source, &snapshot.0, &mut totals).map_err(|_| "cannot export Rust source snapshot")?;
    let uid = id("-u")?;
    let gid = id("-g")?;
    let mut docker = Command::new("docker");
    docker.env_clear();
    for name in [
        "PATH",
        "HOME",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "DOCKER_CERT_PATH",
        "DOCKER_TLS_VERIFY",
    ] {
        if let Some(value) = env::var_os(name) {
            docker.env(name, value);
        }
    }
    let started = Instant::now();
    let name = format!("control-analysis-{}", std::process::id());
    docker.args([
        "run",
        "--rm",
        "--pull=never",
        "--name",
        &name,
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--pids-limit=128",
        "--memory=2g",
        "--cpus=2",
        "--user",
        &format!("{uid}:{gid}"),
        "--tmpfs",
        "/tmp:rw,nosuid,size=128m",
        "--tmpfs",
        &format!("/home/tool:rw,nosuid,uid={uid},gid={gid},size=64m"),
        "--mount",
        &format!(
            "type=bind,src={},dst=/source,readonly",
            snapshot.0.display()
        ),
        "--mount",
        &format!("type=bind,src={},dst=/out", output.display()),
        &args[0],
        &args[3],
    ]);
    docker.stdin(Stdio::null());
    let mut child = docker
        .spawn()
        .map_err(|_| "cannot start Docker; load the Nix image first")?;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|_| "cannot wait for Docker")? {
            break status;
        }
        if started.elapsed().as_secs() >= 300 {
            let _ = child.kill();
            let _ = child.wait();
            // Reuse the filtered Docker environment, never pass a socket into the container.
            let mut cleanup = Command::new("docker");
            cleanup
                .env_clear()
                .envs(docker.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))));
            let _ = cleanup
                .args(["rm", "--force", &name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            return Err("five-minute limit exceeded; container removed".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    println!("{} source files; reports in {}", totals.0, output.display());
    Ok(status.success())
}

fn id(flag: &str) -> Result<String, String> {
    let output = Command::new("id")
        .arg(flag)
        .output()
        .map_err(|_| "cannot determine local user")?;
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err("cannot determine local user".into());
    }
    if flag == "-u" && value == "0" {
        return Err("run the launcher as an ordinary user".into());
    }
    Ok(value)
}

fn valid_image(image: &str) -> bool {
    image
        .strip_prefix("sha256:")
        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn include_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("Cargo.toml" | "Cargo.lock")
    ) || path.extension().is_some_and(|ext| ext == "rs")
}

fn export(source: &Path, destination: &Path, totals: &mut (usize, u64)) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name
            .to_str()
            .is_none_or(|n| n.starts_with('.') || matches!(n, "target" | "node_modules" | "dist"))
        {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let target = destination.join(&name);
        if kind.is_dir() {
            fs::create_dir(&target)?;
            export(&entry.path(), &target, totals)?;
        } else if kind.is_file() && include_file(&entry.path()) {
            totals.0 += 1;
            totals.1 += entry.metadata()?.len();
            if totals.0 > 50_000 || totals.1 > 256 * 1024 * 1024 {
                return Err(io::Error::other("snapshot exceeds limit"));
            }
            fs::copy(entry.path(), &target)?;
            fs::set_permissions(target, fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

struct Snapshot(PathBuf);
impl Snapshot {
    fn new() -> Result<Self, String> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_nanos();
        let path = env::temp_dir().join(format!("control-analysis-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).map_err(|_| "cannot create source snapshot")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot protect source snapshot")?;
        Ok(Self(path))
    }
}
impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn analyze(check: &str) -> Result<bool, String> {
    let arguments: &[&str] = match check {
        "fmt" => &["fmt", "--all", "--", "--check"],
        "clippy" => &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--offline",
            "--",
            "-D",
            "warnings",
        ],
        _ => return Err("unknown check".into()),
    };
    let started = Instant::now();
    let log = fs::File::create("/out/report.txt").map_err(|_| "cannot create report")?;
    let stderr = log.try_clone().map_err(|_| "cannot create report")?;
    let status = Command::new("cargo")
        .args(arguments)
        .current_dir("/source")
        .env("CARGO_NET_OFFLINE", "true")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(stderr)
        .status()
        .map_err(|_| "cannot run Cargo")?;
    let report = format!(
        "{{\"check\":\"{check}\",\"success\":{},\"exit_code\":{},\"elapsed_ms\":{}}}\n",
        status.success(),
        status.code().unwrap_or(-1),
        started.elapsed().as_millis()
    );
    fs::write("/out/result.json", report).map_err(|_| "cannot save result")?;
    println!(
        "{check}: {}",
        if status.success() {
            "passed"
        } else {
            "failed (see report.txt)"
        }
    );
    Ok(status.success())
}

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
