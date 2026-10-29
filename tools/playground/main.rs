//! PID 1 for a credential-free, network-free Rust snippet container.
use std::{fs, io::{self, Read, Write}, process::{Command, Stdio}, time::{Duration, Instant}};

fn log(path: &str) -> io::Result<fs::File> {
    fs::OpenOptions::new().write(true).create_new(true).open(path)
}
fn output(path: &str, limit: usize) -> String {
    use std::os::unix::fs::OpenOptionsExt;
    let read = || -> io::Result<Vec<u8>> {
        let f = fs::OpenOptions::new().read(true).custom_flags(0x20000 | 0x800).open(path)?;
        if !f.metadata()?.is_file() { return Err(io::Error::other("Invalid log")); }
        let mut bytes = Vec::new();
        f.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let Ok(bytes) = read() else { return "[output unavailable]".into(); };
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(limit)]).into_owned();
    if bytes.len() > limit { text.push_str("\n[output truncated]"); }
    text
}
fn run(command: &mut Command, path: &str, seconds: u64) -> io::Result<(bool, Option<i32>)> {
    let file = log(path)?;
    let mut child = command.stdin(Stdio::null()).stdout(file.try_clone()?).stderr(file).spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? { return Ok((false, status.code())); }
        if start.elapsed() >= Duration::from_secs(seconds) {
            let _ = child.kill(); let _ = child.wait();
            return Ok((true, None));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn main() -> io::Result<()> {
    let mut code = Vec::new();
    io::stdin().take(16385).read_to_end(&mut code)?;
    if code.is_empty() || code.len() > 16384 || std::str::from_utf8(&code).is_err() {
        return Err(io::Error::other("Expected UTF-8 Rust source, at most 16 KiB"));
    }
    fs::write("/tmp/main.rs", code)?;
    let (timeout, exit) = run(Command::new("rustc").args(["--edition=2024", "--color=never", "/tmp/main.rs", "-o", "/tmp/snippet"]), "/tmp/compile.log", 12)?;
    if timeout || exit != Some(0) {
        println!("compile {}", if timeout {"timeout".into()} else {exit.map_or("signal".into(), |c| c.to_string())});
        print!("{}", output("/tmp/compile.log", 8192));
    } else {
        let (timeout, exit) = run(&mut Command::new("/tmp/snippet"), "/tmp/run.log", 3)?;
        println!("run {}", if timeout {"timeout".into()} else {exit.map_or("signal".into(), |c| c.to_string())});
        let warnings = output("/tmp/compile.log", 8192);
        if !warnings.is_empty() { println!("{warnings}"); }
        print!("{}", output("/tmp/run.log", 32768));
    }
    io::stdout().flush()
}
