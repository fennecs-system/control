//! Trusted lease/heartbeat-bounded relay inside an offline service.
mod protocol;
use protocol::{Kind, LIMIT};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn prctl(option: i32, ...) -> i32;
}
fn run() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    if unsafe { prctl(4, 0usize, 0usize, 0usize, 0usize) } != 0 {
        return Err(io::Error::other("Could not protect relay"));
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err(io::Error::other(
            "Expected mode, loopback port and deadline",
        ));
    }
    let port = args[1]
        .parse::<u16>()
        .ok()
        .filter(|p| *p != 0)
        .ok_or_else(|| io::Error::other("Invalid port"))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs();
    let seconds = args[2]
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_sub(now))
        .filter(|n| (1..=3600).contains(n))
        .ok_or_else(|| io::Error::other("Expired relay lease"))?;
    let start = Instant::now();
    let heartbeat = Arc::new(AtomicU64::new(0));
    let clock = heartbeat.clone();
    std::thread::spawn(move || {
        loop {
            let elapsed = start.elapsed();
            if elapsed >= Duration::from_secs(seconds)
                || elapsed.as_millis() as u64 > clock.load(Ordering::Acquire) + 5000
            {
                std::process::exit(124);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        let mut buffer = [0; LIMIT];
        let mut fin = false;
        loop {
            match protocol::read(&mut input, &mut buffer) {
                Ok((Kind::Ping, _)) => {}
                Ok((Kind::Data, n)) if !fin => {
                    if tx.send(Some(buffer[..n].to_vec())).is_err() {
                        std::process::exit(125);
                    }
                }
                Ok((Kind::Fin, _)) if !fin => {
                    fin = true;
                    if tx.send(None).is_err() {
                        std::process::exit(125);
                    }
                }
                _ => std::process::exit(125),
            }
            heartbeat.store(start.elapsed().as_millis() as u64, Ordering::Release);
        }
    });
    let stream = match args[0].as_str() {
        "connect" => {
            let s =
                TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2))?;
            println!("CONTROL_RELAY_READY");
            io::stdout().flush()?;
            s
        }
        "listen" => {
            let listener = TcpListener::bind(("127.0.0.1", port))?;
            listener.set_nonblocking(true)?;
            println!("CONTROL_RELAY_READY");
            io::stdout().flush()?;
            loop {
                match listener.accept() {
                    Ok((s, _)) => {
                        println!("CONTROL_RELAY_CONNECTED");
                        io::stdout().flush()?;
                        break s;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(25))
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        _ => return Err(io::Error::other("Invalid mode")),
    };
    stream.set_nodelay(true)?;
    let done_read = Arc::new(AtomicBool::new(false));
    let done_write = Arc::new(AtomicBool::new(false));
    let mut reading = stream.try_clone()?;
    let a = done_read.clone();
    std::thread::spawn(move || {
        let mut output = io::stdout().lock();
        let mut buffer = [0; LIMIT];
        loop {
            match reading.read(&mut buffer) {
                Ok(0) => {
                    let _ = protocol::write(&mut output, Kind::Fin, &[]);
                    a.store(true, Ordering::Release);
                    break;
                }
                Ok(n) => {
                    if protocol::write(&mut output, Kind::Data, &buffer[..n]).is_err() {
                        std::process::exit(125);
                    }
                }
                Err(_) => std::process::exit(125),
            }
        }
    });
    let b = done_write.clone();
    std::thread::spawn(move || {
        let mut stream = stream;
        for data in rx {
            match data {
                Some(data) => {
                    if stream.write_all(&data).is_err() {
                        std::process::exit(125);
                    }
                }
                None => {
                    let _ = stream.shutdown(Shutdown::Write);
                    b.store(true, Ordering::Release);
                    break;
                }
            }
        }
    });
    while !done_read.load(Ordering::Acquire) || !done_write.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(125),
    }
}
