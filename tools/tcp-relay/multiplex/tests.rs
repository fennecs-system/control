
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
    assert!(
        !pair.listener.has_capacity(),
        "a full transport admits no more streams"
    );
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
    assert!(
        pair.listener.has_capacity(),
        "closed streams free their slots for waiting connections"
    );
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
    // A heartbeat newer than the ping stays newer, so a descheduled poller
    // cannot miss it the way a fixed freshness window can under load.
    let sent = Instant::now();
    pair.host.send(0, Kind::Ping, &[]).unwrap();
    while *pair.listener.heartbeat.lock().unwrap() < sent {
        assert!(sent.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(5));
    }
    drop(pair);
    blocked.join().unwrap();
}
#[test]
fn a_stream_transfers_more_than_its_credit_window() {
    {
        let pair = Pair::new();
        let (mut client, mut server) = pair.connect();
        let payload = vec![9u8; LIMIT * (WINDOW as usize + 3)];
        let expected = payload.clone();
        let sender = thread::spawn(move || {
            server.write_all(&payload).unwrap();
            server.shutdown(Shutdown::Write).unwrap();
        });
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut received = Vec::new();
        client.read_to_end(&mut received).unwrap();
        sender.join().unwrap();
        assert_eq!(received, expected);
    }
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
    let (_link, incoming, reader) =
        Transport::start(std::io::Cursor::new(bytes), output.clone(), true);
    reader.join().unwrap();
    assert_eq!(
        incoming.try_iter().count(),
        CONNECTIONS,
        "every stream within the limit is admitted"
    );
    let written = output.0.lock().unwrap().clone();
    assert!(
        written.starts_with(&(CONNECTIONS as u32 + 1).to_be_bytes()),
        "only the excess stream is closed"
    );
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
