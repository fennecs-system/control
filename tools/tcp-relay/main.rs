//! Trusted lease/heartbeat-bounded relay inside an offline service.
mod protocol;
mod multiplex;
use protocol::{Credit, Kind, LIMIT};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    process::ExitCode,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn prctl(option: i32, ...) -> i32;
}
fn abort() -> ! {
    std::process::exit(125)
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
        .filter(|n| (1..=3600).contains(n) || args[2] == u64::MAX.to_string())
        .ok_or_else(|| io::Error::other("Expired relay lease"))?;
    if args[0]=="listen-multi" {return multiplex_listener(port,Duration::from_secs(seconds));}
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
    let credit = Arc::new(Credit::default());
    let outgoing = credit.clone();
    let may_receive = Arc::new(AtomicBool::new(false));
    let permission = may_receive.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Option<Vec<u8>>>(1);
    // Parsing never performs TCP writes or blocks on the application queue.
    // One advertised credit permits exactly one Data or FIN frame.
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        let mut buffer = [0; LIMIT];
        let mut fin = false;
        loop {
            match protocol::read(&mut input, &mut buffer) {
                Ok((Kind::Ping, _)) => {
                    heartbeat.store(start.elapsed().as_millis() as u64, Ordering::Release)
                }
                Ok((Kind::Credit, _)) => {
                    if outgoing.grant().is_err() {
                        abort();
                    }
                }
                Ok((Kind::Data, n)) if !fin && permission.swap(false, Ordering::AcqRel) => {
                    if tx.try_send(Some(buffer[..n].to_vec())).is_err() {
                        abort();
                    }
                }
                Ok((Kind::Fin, _)) if !fin && permission.swap(false, Ordering::AcqRel) => {
                    fin = true;
                    if tx.try_send(None).is_err() {
                        abort();
                    }
                }
                _ => abort(),
            }
        }
    });
    let stream = match args[0].as_str() {
        "connect" => {
            let stream =
                TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2))?;
            println!("CONTROL_RELAY_V2_READY");
            io::stdout().flush()?;
            stream
        }
        "listen" => {
            let listener = TcpListener::bind(("127.0.0.1", port))?;
            listener.set_nonblocking(true)?;
            println!("CONTROL_RELAY_V2_READY");
            io::stdout().flush()?;
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        println!("CONTROL_RELAY_V2_CONNECTED");
                        io::stdout().flush()?;
                        break stream;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(25))
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        _ => return Err(io::Error::other("Invalid mode")),
    };
    stream.set_nodelay(true)?;
    let output = Arc::new(Mutex::new(io::stdout()));
    may_receive.store(true, Ordering::Release);
    protocol::write(
        &mut *output
            .lock()
            .map_err(|_| io::Error::other("Relay output unavailable"))?,
        Kind::Credit,
        &[],
    )?;
    let done_read = Arc::new(AtomicBool::new(false));
    let done_write = Arc::new(AtomicBool::new(false));
    let mut reading = stream.try_clone()?;
    let a = done_read.clone();
    let writer = output.clone();
    std::thread::spawn(move || {
        let mut buffer = [0; LIMIT];
        let alive = AtomicBool::new(true);
        loop {
            if credit.take(&alive).is_err() {
                abort();
            }
            let (kind, n) = match reading.read(&mut buffer) {
                Ok(0) => (Kind::Fin, 0),
                Ok(n) => (Kind::Data, n),
                Err(_) => abort(),
            };
            if writer
                .lock()
                .map_err(|_| io::Error::other("Relay output unavailable"))
                .and_then(|mut out| protocol::write(&mut *out, kind, &buffer[..n]))
                .is_err()
            {
                abort();
            }
            if kind == Kind::Fin {
                a.store(true, Ordering::Release);
                break;
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
                        abort();
                    }
                    may_receive.store(true, Ordering::Release);
                    if output
                        .lock()
                        .map_err(|_| io::Error::other("Relay output unavailable"))
                        .and_then(|mut out| protocol::write(&mut *out, Kind::Credit, &[]))
                        .is_err()
                    {
                        abort();
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
fn multiplex_listener(port:u16,lifetime:Duration) -> io::Result<()> {
    let listener=TcpListener::bind(("127.0.0.1",port))?;listener.set_nonblocking(true)?;
    let (transport,_incoming,_reader)=multiplex::Transport::start(io::stdin(),io::stdout(),false);
    println!("CONTROL_RELAY_V3_READY");io::stdout().flush()?;
    let started=Instant::now();
    while transport.alive() && transport.heartbeat_age()<Duration::from_secs(5) && started.elapsed()<lifetime {
        match listener.accept() {
            Ok((socket,_)) => {let _=transport.open(socket);}
            Err(error) if error.kind()==io::ErrorKind::WouldBlock=>std::thread::sleep(Duration::from_millis(10)),
            Err(error)=>{transport.stop();return Err(error);}
        }
    }
    transport.stop();Err(io::Error::other("Relay expired or lost its host"))
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(125),
    }
}
