use super::*;
use std::{
    io::{BufRead, BufReader, Read},
    net::TcpListener,
};

#[test]
fn stream_refreshes_on_connect_changes_and_cancels_without_another_poll() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = BufReader::new(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
    client.get_ref().set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let changes = Changes::new();
    let cancel = CancelToken::new();
    let (source, stop) = (changes.clone(), cancel.clone());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream_updates(&mut stream, &source, &stop).unwrap();
    });
    let mut header = String::new();
    loop {
        let mut line = String::new();
        client.read_line(&mut line).unwrap();
        header.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    assert!(header.contains("Content-Type: text/event-stream"));
    let mut data = [0; 15];
    client.read_exact(&mut data).unwrap();
    assert_eq!(&data, b"data: changed\n\n");
    changes.bump();
    client.read_exact(&mut data).unwrap();
    assert_eq!(&data, b"data: changed\n\n");
    cancel.cancel();
    server.join().unwrap();
    assert_eq!(client.read(&mut data).unwrap(), 0);
}

#[test]
fn subscription_limit_is_released_on_drop() {
    let guards: Vec<_> = (0..MAX_SUBSCRIBERS).map(|_| Subscriber::acquire().unwrap()).collect();
    assert!(Subscriber::acquire().is_none());
    drop(guards);
    assert!(Subscriber::acquire().is_some());
}

#[test]
fn in_memory_progress_hints_at_most_once_per_interval() {
    let start = Instant::now();
    let mut last = None;
    assert!(progress(&mut last, start), "the first progress is shown at once");
    assert!(!progress(&mut last, start + PROGRESS_INTERVAL / 2));
    assert_eq!(last, Some(start), "a suppressed hint keeps the last time");
    assert!(progress(&mut last, start + PROGRESS_INTERVAL));
    assert_eq!(last, Some(start + PROGRESS_INTERVAL));
}
