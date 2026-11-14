use super::*;
use std::{
    io::{BufRead, BufReader, Read},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

fn connection() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    (client, listener.accept().unwrap().0)
}

#[test]
fn silent_clients_are_bounded_before_spawn_and_capacity_recovers() {
    let mut requests = Requests::new();
    let active = Arc::new(AtomicUsize::new(0));
    let mut clients = Vec::new();
    let (started, arrivals) = mpsc::channel();
    for _ in 0..MAX_HANDLERS {
        let (client, server) = connection();
        let (active, started) = (active.clone(), started.clone());
        assert!(
            requests
                .dispatch(server, move |mut stream| {
                    active.fetch_add(1, Ordering::SeqCst);
                    started.send(()).unwrap();
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    active.fetch_sub(1, Ordering::SeqCst);
                })
                .unwrap()
        );
        clients.push(client);
    }
    for _ in 0..MAX_HANDLERS {
        arrivals.recv_timeout(Duration::from_secs(3)).unwrap();
    }
    assert_eq!(active.load(Ordering::SeqCst), MAX_HANDLERS);
    for _ in 0..64 {
        let (client, server) = connection();
        assert!(
            !requests
                .dispatch(server, |_| panic!("overload spawned a handler"))
                .unwrap()
        );
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        assert_eq!(line, "HTTP/1.1 503 Service Unavailable\r\n");
        assert_eq!(requests.tasks.len(), MAX_HANDLERS);
    }
    clients[0].write_all(b"x").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !requests.tasks[0].is_finished() {
        assert!(Instant::now() < deadline, "completed handler did not exit");
        std::thread::yield_now();
    }
    let (client, server) = connection();
    assert!(
        requests
            .dispatch(server, |mut stream| {
                stream.write_all(b"recovered").unwrap();
            })
            .unwrap()
    );
    let mut body = String::new();
    BufReader::new(client).read_to_string(&mut body).unwrap();
    assert_eq!(body, "recovered");
    for mut client in clients.into_iter().skip(1) {
        client.write_all(b"x").unwrap();
    }
    requests.join().unwrap();
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[test]
fn completed_and_panicked_handlers_release_slots_but_panics_remain_visible() {
    let mut requests = Requests::new();
    let (_client, server) = connection();
    requests
        .dispatch(server, |_| panic!("synthetic handler failure"))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !requests.tasks[0].is_finished() {
        assert!(Instant::now() < deadline, "panicked handler did not exit");
        std::thread::yield_now();
    }
    let (client, server) = connection();
    assert!(
        requests
            .dispatch(server, |mut stream| {
                stream.write_all(b"ok").unwrap();
            })
            .unwrap()
    );
    assert!(
        requests.failed,
        "reaping cannot erase the handler's failure"
    );
    let mut response = String::new();
    BufReader::new(client)
        .read_to_string(&mut response)
        .unwrap();
    assert_eq!(response, "ok");
    assert!(requests.join().is_err());
}

#[test]
fn long_lived_streams_count_toward_capacity_and_join_waits_for_cancellation() {
    let mut requests = Requests::new();
    let cancel = control_effect::sync::CancelToken::new();
    let (client, server) = connection();
    let stop = cancel.clone();
    requests
        .dispatch(server, move |mut stream| {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: ready\n\n",
                )
                .unwrap();
            stop.wait(None);
        })
        .unwrap();
    let mut client = BufReader::new(client);
    let mut line = String::new();
    client.read_line(&mut line).unwrap();
    assert_eq!(line, "HTTP/1.1 200 OK\r\n");
    assert_eq!(requests.tasks.len(), 1);
    let (finished, done) = mpsc::channel();
    let join = std::thread::spawn(move || {
        requests.join().unwrap();
        finished.send(()).unwrap();
    });
    assert!(done.recv_timeout(Duration::from_millis(25)).is_err());
    cancel.cancel();
    done.recv_timeout(Duration::from_secs(3)).unwrap();
    join.join().unwrap();
}

#[test]
fn overload_does_not_wait_for_a_nonreading_peer() {
    let mut requests = Requests::new();
    let cancel = control_effect::sync::CancelToken::new();
    let mut clients = Vec::new();
    for _ in 0..MAX_HANDLERS {
        let (client, server) = connection();
        let stop = cancel.clone();
        requests
            .dispatch(server, move |_| {
                stop.wait(Some(Instant::now() + Duration::from_secs(10)));
            })
            .unwrap();
        clients.push(client);
    }
    let (_client, mut server) = connection();
    server.set_nonblocking(true).unwrap();
    let block = [0u8; 65536];
    loop {
        match server.write(&block) {
            Ok(0) => panic!("connection closed"),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("{error}"),
        }
    }
    // The real admission path must not wait for an already full send buffer.
    let start = Instant::now();
    assert!(
        !requests
            .dispatch(server, |_| panic!("overload spawned a handler"))
            .unwrap()
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    cancel.cancel();
    requests.join().unwrap();
}
