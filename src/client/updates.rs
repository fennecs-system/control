//! Cancellable, coalesced loopback update hints, independent of terminal code.
use super::{Hive, local_server};
use control_effect::sync::{CancelToken, Changes, Newer};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub struct Updates {
    changed: Arc<Changes>,
    connected: Arc<AtomicBool>,
    cancel: CancelToken,
    socket: Arc<Mutex<Option<TcpStream>>>,
    thread: Option<JoinHandle<()>>,
    seen: u64,
}
impl Hive {
    pub fn updates(&self) -> io::Result<Updates> {
        let origin = reqwest::Url::parse(&self.endpoint)
            .map_err(io::Error::other)?
            .join("/")
            .map_err(io::Error::other)?;
        let origin = local_server(origin.as_str())?;
        let host = origin.host_str().unwrap();
        let ip = if host == "[::1]" {
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        } else {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        };
        let port = origin.port_or_known_default().unwrap();
        let address = SocketAddr::new(ip, port);
        let request = format!(
            "GET /events HTTP/1.1\r\nHost: {host}:{port}\r\nCookie: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n",
            self.operator_cookie.to_str().map_err(io::Error::other)?
        );
        let changed = Changes::new();
        let connected = Arc::new(AtomicBool::new(false));
        let cancel = CancelToken::new();
        let socket = Arc::new(Mutex::new(None));
        let (signal, online, stop, connection) = (
            changed.clone(),
            connected.clone(),
            cancel.clone(),
            socket.clone(),
        );
        let thread = std::thread::Builder::new()
            .name("control-updates".into())
            .spawn(move || {
                while !stop.is_cancelled() {
                    let _ = receive(address, &request, &connection, &stop, &online, &signal);
                    online.store(false, Ordering::Release);
                    connection.lock().unwrap_or_else(|e| e.into_inner()).take();
                    stop.wait(Some(Instant::now() + Duration::from_secs(1)));
                }
            })?;
        Ok(Updates {
            changed,
            connected,
            cancel,
            socket,
            thread: Some(thread),
            seen: 0,
        })
    }
}
impl Updates {
    /// A single hint covers every update since the previous read.
    pub fn changed(&mut self) -> bool {
        let generation = self.changed.generation();
        let changed = generation != self.seen;
        self.seen = generation;
        changed
    }
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }
    pub fn wait(&mut self, maximum: Duration) -> bool {
        match self
            .changed
            .wait_newer(self.seen, Some(Instant::now() + maximum), &self.cancel)
        {
            Newer::Changed(generation) => {
                self.seen = generation;
                true
            }
            _ => false,
        }
    }
}
impl Drop for Updates {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(socket) = self
            .socket
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn receive(
    address: SocketAddr,
    request: &str,
    connection: &Mutex<Option<TcpStream>>,
    stop: &CancelToken,
    online: &AtomicBool,
    changed: &Changes,
) -> io::Result<()> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    // Install under the same lock Drop uses, then recheck cancellation: Drop
    // cannot miss a newly opened socket and leave the reader blocked.
    *connection.lock().unwrap_or_else(|e| e.into_inner()) = Some(stream.try_clone()?);
    if stop.is_cancelled() {
        return Ok(());
    }
    stream.write_all(request.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut header = String::new();
    loop {
        let mut line = String::new();
        if reader
            .by_ref()
            .take(8193 - header.len() as u64)
            .read_line(&mut line)?
            == 0
        {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        header.push_str(&line);
        if header.len() > 8192 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if line == "\r\n" {
            break;
        }
    }
    if !header.starts_with("HTTP/1.1 200 ")
        || !header
            .lines()
            .any(|line| line.eq_ignore_ascii_case("Content-Type: text/event-stream"))
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(20)))?;
    online.store(true, Ordering::Release);
    while !stop.is_cancelled() {
        let mut line = String::new();
        if reader.by_ref().take(1025).read_line(&mut line)? == 0 {
            break;
        }
        if line.len() > 1024 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if line == "data: changed\n" {
            changed.bump();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
