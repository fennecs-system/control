use super::*;
use std::net::TcpListener;

#[test]
fn shutdown_unblocks_a_live_reader_and_hints_are_coalesced() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, notified) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut line = String::new();
            request.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: changed\n\ndata: changed\n\n").unwrap();
        ready.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let changed = Changes::new();
    let connected = Arc::new(AtomicBool::new(false));
    let socket = Arc::new(Mutex::new(None));
    let cancel = CancelToken::new();
    let (signal, online, connection, stop) = (
        changed.clone(),
        connected.clone(),
        socket.clone(),
        cancel.clone(),
    );
    let reader = std::thread::spawn(move || {
        receive(
            address,
            "GET /events HTTP/1.1\r\nHost: localhost\r\n\r\n",
            &connection,
            &stop,
            &online,
            &signal,
        )
    });
    notified.recv_timeout(Duration::from_secs(2)).unwrap();
    let mut updates = Updates {
        changed,
        connected,
        cancel,
        socket,
        thread: None,
        seen: 0,
    };
    assert!(updates.wait(Duration::from_secs(2)));
    // Reader may process the second hint after the first wake, so consume it
    // before checking coalescing of multiple already-published generations.
    assert!(matches!(
        updates.changed.wait_newer(
            1,
            Some(Instant::now() + Duration::from_secs(2)),
            &updates.cancel
        ),
        Newer::Changed(2)
    ));
    assert!(updates.connected());
    assert!(updates.changed() || updates.seen == 2);
    assert!(!updates.changed());
    drop(updates);
    reader.join().unwrap().unwrap();
    server.join().unwrap();
}

#[test]
fn malformed_or_oversized_stream_is_rejected() {
    for response in [
        b"HTTP/1.1 403 Forbidden\r\nContent-Type: text/event-stream\r\n\r\n".to_vec(),
        vec![b'x'; 9000],
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.write_all(&response);
        });
        let online = AtomicBool::new(false);
        assert!(
            receive(
                address,
                "GET /events HTTP/1.1\r\n\r\n",
                &Mutex::new(None),
                &CancelToken::new(),
                &online,
                &Changes::new()
            )
            .is_err()
        );
        assert!(!online.load(Ordering::Acquire));
        server.join().unwrap();
    }
}
