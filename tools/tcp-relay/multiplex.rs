//! Bounded independent TCP streams over one trusted stdio connection.
//! The parser never writes to TCP or waits for a stream's payload queue.
use super::protocol::{self, Credit, Kind, LIMIT};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::{Shutdown, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const CONNECTIONS: usize = 32;
type Payload = Option<Vec<u8>>;
struct Channel {
    outgoing: Credit,
    receiving: AtomicBool,
    fin: AtomicBool,
    alive: AtomicBool,
    read_done: AtomicBool,
    write_done: AtomicBool,
    input: SyncSender<Payload>,
    socket: Mutex<Option<TcpStream>>,
}
pub struct Incoming {
    id: u32,
    channel: Arc<Channel>,
    input: Receiver<Payload>,
}
pub struct Transport {
    output: Mutex<Box<dyn Write + Send>>,
    channels: Mutex<HashMap<u32, Arc<Channel>>>,
    alive: AtomicBool,
    last_id: AtomicU32,
    heartbeat: Mutex<Instant>,
}
fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
impl Transport {
    pub fn start(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        accept_remote: bool,
    ) -> (Arc<Self>, Receiver<Incoming>, JoinHandle<()>) {
        let transport = Arc::new(Self {
            output: Mutex::new(Box::new(writer)),
            channels: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
            last_id: AtomicU32::new(0),
            heartbeat: Mutex::new(Instant::now()),
        });
        let (tx, rx) = mpsc::sync_channel(CONNECTIONS);
        let link = transport.clone();
        let task = thread::spawn(move || {
            let _ = link.read(reader, accept_remote, tx);
            link.stop();
        });
        (transport, rx, task)
    }
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
    /// Whether another stream fits; listeners leave further connections in
    /// their accept backlog until one closes instead of resetting them.
    #[allow(dead_code)] // Listener-side API; shared with the host binary.
    pub fn has_capacity(&self) -> bool {
        self.channels.lock().is_ok_and(|channels| channels.len() < CONNECTIONS)
    }
    #[allow(dead_code)] // Listener-side API; shared with the host binary.
    pub fn heartbeat_age(&self) -> Duration {
        self.heartbeat
            .lock()
            .map_or(Duration::MAX, |time| time.elapsed())
    }
    pub fn send(&self, id: u32, kind: Kind, data: &[u8]) -> io::Result<()> {
        if !self.alive() {
            return Err(fail("Multiplex relay closed"));
        }
        let mut writer = self
            .output
            .lock()
            .map_err(|_| fail("Relay writer unavailable"))?;
        writer.write_all(&id.to_be_bytes())?;
        protocol::write(&mut *writer, kind, data)
    }
    fn reserve(&self, id: u32) -> io::Result<Incoming> {
        let mut channels = self
            .channels
            .lock()
            .map_err(|_| fail("Relay streams unavailable"))?;
        if id == 0 || channels.len() >= CONNECTIONS || channels.contains_key(&id) {
            return Err(fail("Relay stream limit or duplicate ID"));
        }
        let (tx, rx) = mpsc::sync_channel(1);
        let channel = Arc::new(Channel {
            outgoing: Credit::default(),
            receiving: AtomicBool::new(false),
            fin: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            read_done: AtomicBool::new(false),
            write_done: AtomicBool::new(false),
            input: tx,
            socket: Mutex::new(None),
        });
        channels.insert(id, channel.clone());
        Ok(Incoming {
            id,
            channel,
            input: rx,
        })
    }
    #[allow(dead_code)] // Listener-side API; shared with the host binary.
    pub fn open(self: &Arc<Self>, socket: TcpStream) -> io::Result<()> {
        let previous = self
            .last_id
            .try_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
            .map_err(|_| fail("Relay stream IDs exhausted"))?;
        let incoming = self.reserve(previous + 1)?;
        if let Err(error) = self.send(incoming.id, Kind::Open, &[]) {
            self.close(incoming.id, false);
            return Err(error);
        }
        self.attach(incoming, socket)
    }
    #[allow(dead_code)] // Host-side API; shared with the container binary.
    pub fn reject(&self, incoming: Incoming) {
        self.close(incoming.id, true);
    }
    pub fn attach(self: &Arc<Self>, incoming: Incoming, mut socket: TcpStream) -> io::Result<()> {
        let Incoming { id, channel, input } = incoming;
        let result = (|| {
            if !self.alive() || !channel.alive.load(Ordering::Acquire) {
                return Err(fail("Relay stream closed"));
            }
            socket.set_nonblocking(false)?;
            socket.set_nodelay(true)?;
            *channel
                .socket
                .lock()
                .map_err(|_| fail("Relay socket unavailable"))? = Some(socket.try_clone()?);
            let mut reading = socket.try_clone()?;
            channel.receiving.store(true, Ordering::Release);
            self.send(id, Kind::Credit, &[])?;
            let link = self.clone();
            let stream = channel.clone();
            thread::spawn(move || {
                let mut buffer = [0; LIMIT];
                let result = (|| -> io::Result<()> {
                    loop {
                        stream.outgoing.take(&stream.alive)?;
                        let n = reading.read(&mut buffer)?;
                        link.send(
                            id,
                            if n == 0 { Kind::Fin } else { Kind::Data },
                            &buffer[..n],
                        )?;
                        if n == 0 {
                            stream.read_done.store(true, Ordering::Release);
                            return Ok(());
                        }
                    }
                })();
                if result.is_err() {
                    link.close(id, true);
                } else {
                    link.finish(id, &stream);
                }
            });
            let link = self.clone();
            let stream = channel.clone();
            thread::spawn(move || {
                let result = (|| -> io::Result<()> {
                    loop {
                        match input.recv_timeout(Duration::from_millis(100)) {
                            Ok(Some(data)) => {
                                socket.write_all(&data)?;
                                stream.receiving.store(true, Ordering::Release);
                                link.send(id, Kind::Credit, &[])?;
                            }
                            Ok(None) => {
                                socket.shutdown(Shutdown::Write)?;
                                stream.write_done.store(true, Ordering::Release);
                                return Ok(());
                            }
                            Err(mpsc::RecvTimeoutError::Timeout)
                                if stream.alive.load(Ordering::Acquire) => {}
                            Err(_) => return Err(fail("Relay stream closed")),
                        }
                    }
                })();
                if result.is_err() {
                    link.close(id, true);
                } else {
                    link.finish(id, &stream);
                }
            });
            Ok(())
        })();
        if result.is_err() {
            self.close(id, true);
        }
        result
    }
    fn finish(&self, id: u32, channel: &Channel) {
        if channel.read_done.load(Ordering::Acquire) && channel.write_done.load(Ordering::Acquire) {
            self.close(id, true);
        }
    }
    fn close(&self, id: u32, notify: bool) {
        let channel = self
            .channels
            .lock()
            .ok()
            .and_then(|mut channels| channels.remove(&id));
        if let Some(channel) = channel {
            channel.alive.store(false, Ordering::Release);
            if let Ok(socket) = channel.socket.lock()
                && let Some(socket) = socket.as_ref()
            {
                let _ = socket.shutdown(Shutdown::Both);
            }
            if notify {
                let _ = self.send(id, Kind::Close, &[]);
            }
        }
    }
    pub fn stop(&self) {
        self.alive.store(false, Ordering::Release);
        if let Ok(mut channels) = self.channels.lock() {
            for (_, channel) in channels.drain() {
                channel.alive.store(false, Ordering::Release);
                if let Ok(socket) = channel.socket.lock()
                    && let Some(socket) = socket.as_ref()
                {
                    let _ = socket.shutdown(Shutdown::Both);
                }
            }
        }
    }
    fn read(
        &self,
        mut reader: impl Read,
        accept_remote: bool,
        incoming: SyncSender<Incoming>,
    ) -> io::Result<()> {
        let mut buffer = [0; LIMIT];
        loop {
            let mut id = [0; 4];
            reader.read_exact(&mut id)?;
            let id = u32::from_be_bytes(id);
            let (kind, n) = protocol::read(&mut reader, &mut buffer)?;
            if id == 0 {
                if kind != Kind::Ping {
                    return Err(fail("Invalid relay control frame"));
                }
                *self
                    .heartbeat
                    .lock()
                    .map_err(|_| fail("Relay clock unavailable"))? = Instant::now();
                continue;
            }
            if kind == Kind::Open {
                if !accept_remote || id <= self.last_id.load(Ordering::Acquire) {
                    return Err(fail("Invalid relay stream sequence"));
                }
                self.last_id.store(id, Ordering::Release);
                // A peer with a larger stream limit loses only the excess stream.
                match self.reserve(id) {
                    Ok(stream) => incoming.try_send(stream).map_err(|_| fail("Relay accept queue full"))?,
                    Err(_) => self.send(id, Kind::Close, &[])?,
                }
                continue;
            }
            if kind == Kind::Close {
                self.close(id, false);
                continue;
            }
            let channel = self
                .channels
                .lock()
                .map_err(|_| fail("Relay streams unavailable"))?
                .get(&id)
                .cloned();
            let Some(channel) = channel else {
                // An in-flight credit can follow a completed stream's close.
                if id <= self.last_id.load(Ordering::Acquire)
                    && matches!(kind, Kind::Credit | Kind::Data | Kind::Fin)
                {
                    continue;
                }
                return Err(fail("Unknown relay stream"));
            };
            match kind {
                Kind::Credit => channel.outgoing.grant()?,
                Kind::Data | Kind::Fin
                    if !channel.fin.load(Ordering::Acquire)
                        && channel.receiving.swap(false, Ordering::AcqRel) =>
                {
                    let data = if kind == Kind::Fin {
                        channel.fin.store(true, Ordering::Release);
                        None
                    } else {
                        Some(buffer[..n].to_vec())
                    };
                    channel
                        .input
                        .try_send(data)
                        .map_err(|_| fail("Relay stream exceeded credit"))?;
                }
                _ => return Err(fail("Invalid relay stream frame")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    fn sockets() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        (client, listener.accept().unwrap().0)
    }
    struct Pair {
        listener: Arc<Transport>,
        host: Arc<Transport>,
        incoming: Receiver<Incoming>,
        closing: [TcpStream; 2],
        readers: Vec<JoinHandle<()>>,
    }
    impl Pair {
        fn new() -> Self {
            let (a, b) = sockets();
            let closing = [a.try_clone().unwrap(), b.try_clone().unwrap()];
            let (listener, _, left) = Transport::start(a.try_clone().unwrap(), a, false);
            let (host, incoming, right) = Transport::start(b.try_clone().unwrap(), b, true);
            Self {
                listener,
                host,
                incoming,
                closing,
                readers: vec![left, right],
            }
        }
        fn connect(&self) -> (TcpStream, TcpStream) {
            let (client, socket) = sockets();
            self.listener.open(socket).unwrap();
            let incoming = self.incoming.recv_timeout(Duration::from_secs(2)).unwrap();
            let (socket, server) = sockets();
            self.host.attach(incoming, socket).unwrap();
            (client, server)
        }
    }
    impl Drop for Pair {
        fn drop(&mut self) {
            self.listener.stop();
            self.host.stop();
            for socket in &self.closing {
                let _ = socket.shutdown(Shutdown::Both);
            }
            for reader in self.readers.drain(..) {
                let _ = reader.join();
            }
        }
    }
    #[test]
    fn every_stream_slot_transfers_independently_and_preserves_half_close() {
        let pair = Pair::new();
        let mut clients = Vec::new();
        let mut servers = Vec::new();
        for index in 0..CONNECTIONS {
            let (client, mut server) = pair.connect();
            client
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            servers.push(thread::spawn(move || {
                let mut data = Vec::new();
                server.read_to_end(&mut data).unwrap();
                assert_eq!(data, vec![index as u8; LIMIT * 4]);
                server.write_all(&data).unwrap();
                server.shutdown(Shutdown::Write).unwrap();
            }));
            clients.push(client);
        }
        assert!(!pair.listener.has_capacity(), "a full transport admits no more streams");
        let (_, overflow) = sockets();
        assert!(pair.listener.open(overflow).is_err());
        let clients = clients
            .into_iter()
            .enumerate()
            .map(|(index, mut client)| {
                thread::spawn(move || {
                    client.write_all(&vec![index as u8; LIMIT * 4]).unwrap();
                    client.shutdown(Shutdown::Write).unwrap();
                    let mut reply = Vec::new();
                    client.read_to_end(&mut reply).unwrap();
                    assert_eq!(reply, vec![index as u8; LIMIT * 4]);
                })
            })
            .collect::<Vec<_>>();
        for task in clients.into_iter().chain(servers) {
            task.join().unwrap();
        }
        assert!(pair.listener.alive() && pair.host.alive());
        let freed = Instant::now();
        while !pair.listener.has_capacity() && freed.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(pair.listener.has_capacity(), "closed streams free their slots for waiting connections");
        let start = Instant::now();
        while !pair.listener.channels.lock().unwrap().is_empty() {
            assert!(start.elapsed() < Duration::from_secs(1));
            thread::sleep(Duration::from_millis(5));
        }
        // The same transport remains usable after every socket closes.
        let (mut client, mut server) = pair.connect();
        client.write_all(b"next").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut bytes = Vec::new();
        server.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"next");
    }
    #[test]
    fn a_blocked_stream_does_not_block_other_streams_or_heartbeats() {
        let pair = Pair::new();
        let (mut stalled, _unread) = pair.connect();
        let blocked = thread::spawn(move || {
            let _ = stalled.write_all(&vec![42; 8 * 1024 * 1024]);
        });
        thread::sleep(Duration::from_millis(150));
        let (mut client, mut server) = pair.connect();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        server.write_all(b"responsive").unwrap();
        server.shutdown(Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).unwrap();
        assert_eq!(reply, b"responsive");
        pair.host.send(0, Kind::Ping, &[]).unwrap();
        let start = Instant::now();
        while pair.listener.heartbeat_age() > Duration::from_millis(100) {
            assert!(start.elapsed() < Duration::from_secs(1));
            thread::sleep(Duration::from_millis(5));
        }
        drop(pair);
        blocked.join().unwrap();
    }
    #[test]
    fn malformed_or_uncredited_frames_close_the_transport() {
        for (id, kind) in [(0u32, Kind::Open), (1, Kind::Data), (1, Kind::Credit)] {
            let mut bytes = id.to_be_bytes().to_vec();
            protocol::write(
                &mut bytes,
                kind,
                if kind == Kind::Data { b"x" } else { b"" },
            )
            .unwrap();
            let (link, _, reader) = Transport::start(std::io::Cursor::new(bytes), Vec::new(), true);
            reader.join().unwrap();
            assert!(!link.alive());
        }
    }
    #[test]
    fn a_stream_beyond_the_limit_is_closed_alone() {
        #[derive(Clone, Default)]
        struct Shared(Arc<Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut bytes = Vec::new();
        for id in 1..=CONNECTIONS as u32 + 1 {
            bytes.extend_from_slice(&id.to_be_bytes());
            protocol::write(&mut bytes, Kind::Open, b"").unwrap();
        }
        let output = Shared::default();
        let (_link, incoming, reader) = Transport::start(std::io::Cursor::new(bytes), output.clone(), true);
        reader.join().unwrap();
        assert_eq!(incoming.try_iter().count(), CONNECTIONS, "every stream within the limit is admitted");
        let written = output.0.lock().unwrap().clone();
        assert!(written.starts_with(&(CONNECTIONS as u32 + 1).to_be_bytes()), "only the excess stream is closed");
    }
    #[test]
    fn closing_one_connection_keeps_the_listener_and_other_streams_alive() {
        let pair = Pair::new();
        let (client, server) = pair.connect();
        let (mut other, mut upstream) = pair.connect();
        client.shutdown(Shutdown::Both).unwrap();
        drop(server);
        upstream.write_all(b"still-live").unwrap();
        upstream.shutdown(Shutdown::Write).unwrap();
        other
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reply = Vec::new();
        other.read_to_end(&mut reply).unwrap();
        assert_eq!(reply, b"still-live");
        assert!(pair.listener.alive() && pair.host.alive());
    }
}
