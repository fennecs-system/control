//! One verified container listener, with independent bounded TCP sessions.
use super::multiplex_transport::Transport;
use super::*;
const _: () = assert!(MAX_CONNECTIONS == super::multiplex_transport::CONNECTIONS);

struct Process {
    child: Child,
    transport: Arc<Transport>,
    reader: Option<JoinHandle<()>>,
    heartbeat: Option<JoinHandle<()>>,
}
impl Drop for Process {
    fn drop(&mut self) {
        self.transport.stop();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(task) = self.reader.take() {
            let _ = task.join();
        }
        if let Some(task) = self.heartbeat.take() {
            let _ = task.join();
        }
    }
}
fn listen(shared: &Arc<Mutex<State>>, port: &Port, cancel: &Arc<AtomicBool>) -> io::Result<()> {
    let service = live_service(shared, port, cancel)?;
    let binding = crate::services::port_binding(&service, &port.container)?;
    live_service(shared, port, cancel)?;
    let mut child = binding.spawn(
        RelayMode::Multiplex,
        port.service_port,
        port.approval.as_ref().unwrap().expires,
    )?;
    let output = handshake(
        &mut child,
        b"CONTROL_RELAY_V3_READY\n",
        Duration::from_secs(3),
        shared,
        port,
        cancel,
    )?;
    let Some(input) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(fail("Relay input unavailable"));
    };
    let (transport, incoming, reader) = Transport::start(output, input, true);
    let ping = transport.clone();
    let heartbeat = thread::spawn(move || {
        while ping.alive() {
            if ping.send(0, protocol::Kind::Ping, &[]).is_err() {
                ping.stop();
                break;
            }
            thread::sleep(Duration::from_millis(250));
        }
    });
    let process = Process {
        child,
        transport,
        reader: Some(reader),
        heartbeat: Some(heartbeat),
    };
    receipt(shared, port, true, "");
    let mut sessions: Vec<JoinHandle<()>> = Vec::new();
    while process.transport.alive() && live_service(shared, port, cancel).is_ok() {
        sessions.retain(|task| !task.is_finished());
        match incoming.recv_timeout(Duration::from_millis(50)) {
            Ok(connection) => {
                if sessions.len() >= MAX_CONNECTIONS {
                    process.transport.reject(connection);
                    continue;
                }
                let shared = shared.clone();
                let port = port.clone();
                let cancel = cancel.clone();
                let link = process.transport.clone();
                sessions.push(thread::spawn(move || {
                    // Every stream receives the same exact endpoint and live
                    // generation/grant checks; no client-provided target exists.
                    let socket = (|| {
                        live_service(&shared, &port, &cancel)?;
                        if !shared
                            .lock()
                            .is_ok_and(|state| peer_listening(&state, &port))
                        {
                            return Err(fail("Peer ingress is not listening"));
                        }
                        let target = port
                            .approval
                            .as_ref()
                            .and_then(|approval| approval.pinned)
                            .ok_or_else(|| fail("Missing pinned target"))?;
                        let socket = TcpStream::connect_timeout(&target, Duration::from_secs(2))?;
                        live_service(&shared, &port, &cancel)?;
                        Ok(socket)
                    })();
                    match socket {
                        Ok(socket) => {
                            let _ = link.attach(connection, socket);
                        }
                        Err(error) => {
                            link.reject(connection);
                            receipt(&shared, &port, true, &error.to_string());
                        }
                    }
                }));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    drop(process);
    for task in sessions {
        let _ = task.join();
    }
    Err(fail("Relay stopped or lost its host connection"))
}
pub(super) fn egress(
    shared: Arc<Mutex<State>>,
    port: Port,
    cancel: Arc<AtomicBool>,
) -> io::Result<()> {
    while live_service(&shared, &port, &cancel).is_ok() {
        if let Err(error) = listen(&shared, &port, &cancel) {
            receipt(&shared, &port, false, &error.to_string());
        }
        for _ in 0..10 {
            if live_service(&shared, &port, &cancel).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
    receipt(&shared, &port, false, "");
    Ok(())
}
