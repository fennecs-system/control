
use super::*;
#[test]
#[ignore = "isolated Linux fixture with writable /workspace and private home; no model calls"]
fn notification_descriptor_does_not_wait_for_a_log_newline() {
    let id = format!(
        "test-{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    fs::create_dir_all(directory()).unwrap();
    let marker = std::env::temp_dir().join(&id);
    let log = Arc::new(Mutex::new(Log::open(&id)));
    let mut watch = Watch {
        spec: Spec {
            restart: Restart::Transient,
            notifications: true,
            command: format!(
                "test \"$CONTROL_NOTIFY_FD\" = 3 || exit 42\nprintf 'partial diagnostic'\nprintf '%s\\n' '{{\"task\":\"arbitrary-54\",\"status\":\"completed\"}}' >&3\nwhile test ! -f '{}'; do sleep 0.01; done\nprintf ' finished\\n'",
                marker.display()
            ),
        },
        log: log.clone(),
        running: None,
        restarts: VecDeque::new(),
        restart_at: None,
        removed: false,
    };
    start(&id, &mut watch, Instant::now());
    let running = watch.running.take().expect("watch started");
    struct Child(i32);
    impl Drop for Child {
        fn drop(&mut self) {
            unsafe {
                super::super::kill(-self.0, 9);
            }
        }
    }
    let child = Child(running.pid);
    let read = || {
        fs::read(log_path(&id))
            .unwrap_or_default()
            .split(|&b| b == b'\n')
            .filter_map(Frame::decode)
            .map(|(frame, _)| frame)
            .collect::<Vec<_>>()
    };
    let started = Instant::now();
    loop {
        let frames = read();
        if frames
            .iter()
            .any(|frame| matches!(frame.event, Event::Notification(_)))
        {
            assert!(
                !frames
                    .iter()
                    .any(|frame| matches!(frame.event, Event::Line(_))),
                "partial stdout must not prevent the notification"
            );
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "notification never arrived"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    fs::write(&marker, b"release test child").unwrap();
    let mut raw = 0;
    assert_eq!(
        unsafe { super::super::waitpid(child.0, &mut raw, 0) },
        child.0
    );
    let status = ExitStatus::from_raw(raw);
    assert!(status.success());
    running.reader.join().unwrap();
    exited(
        &id,
        Restart::Transient,
        &mut watch.restarts,
        &mut watch.restart_at,
        &mut watch.removed,
        &mut log.lock().unwrap(),
        status.code(),
        status.signal(),
        Instant::now(),
    );
    let frames = read();
    assert!(matches!(frames[0].event, Event::Notification(_)));
    assert!(matches!(&frames[1].event,Event::Line(bytes) if bytes==b"partial diagnostic finished"));
    assert!(matches!(frames[2].event, Event::Exit { code: Some(0), .. }));
    assert_eq!(frames.len(), 3);
    fs::remove_file(marker).unwrap();
    fs::remove_file(log_path(&id)).unwrap();
}

#[cfg(test)]
#[test]
#[ignore = "isolated Linux collector timing/sampling fixture; no models"]
fn collector_profile() {
    fs::create_dir_all(directory()).unwrap();
    let batches = std::env::var("CONTROL_WATCH_PROFILE_BATCHES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(8)
        .clamp(1, 1024);
    let started = Instant::now();
    for batch in 0..batches {
        let id = format!("bench-{}-{batch}", std::process::id());
        let spec = Spec::decode(
            "transient\nfor ((i=0; i<20000; i++)); do printf 'status unchanged %s\\n' \"$i\"; done",
        )
        .unwrap();
        let log = Arc::new(Mutex::new(Log::open(&id)));
        let mut watch = Watch {
            spec,
            log: log.clone(),
            running: None,
            restarts: VecDeque::new(),
            restart_at: None,
            removed: false,
        };
        start(&id, &mut watch, Instant::now());
        let running = watch.running.take().unwrap();
        let mut raw = 0;
        assert_eq!(
            unsafe { super::super::waitpid(running.pid, &mut raw, 0) },
            running.pid
        );
        assert!(ExitStatus::from_raw(raw).success());
        running.reader.join().unwrap();
        assert_eq!(log.lock().unwrap().seq, 20000);
        fs::remove_file(log_path(&id)).unwrap();
        let _ = fs::remove_file(previous_log(&id));
    }
    println!(
        "COLLECTOR_PROFILE lines={} wall_ns={}",
        u64::from(batches) * 20000,
        started.elapsed().as_nanos()
    );
}
