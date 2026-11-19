//! One HTTP forward proxy per service with approved egress, so standard
//! clients (npm, pip, cargo, git, curl) reach approved hosts through
//! `HTTPS_PROXY` without pinning SNI or rewriting lockfiles.
//!
//! The service dials `127.0.0.1:3129` inside its container; the unchanged
//! `listen-multi` relay carries each stream to the host, where this module reads
//! the request head as stream *data* (the relay protocol still carries no
//! destinations). `CONNECT host:port` tunnels bytes end to end, so TLS is never
//! terminated or inspected. A plain `http://` absolute-URI request is relayed
//! only to port 80. Either is admitted only when the service holds an approved,
//! live egress grant for exactly that host and port, and connects only to that
//! grant's pinned address. Other targets get a 403 naming the fix. A tunnel
//! closes when its grant is revoked or expires, like a port's own relay.
use super::multiplex_transport::{Incoming, Transport};
use super::*;

/// The proxy's loopback port inside every service with approved egress.
pub(crate) const PROXY_PORT: NonZeroU16 = NonZeroU16::new(3129).unwrap();
const PROXY_URL: &str = "http://127.0.0.1:3129";
const HEAD_LIMIT: usize = 8 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
/// Proxy settings exec commands and watches get while the service has egress.
const ENV: [(&str, &str); 6] = [
    ("HTTPS_PROXY", PROXY_URL),
    ("HTTP_PROXY", PROXY_URL),
    ("https_proxy", PROXY_URL),
    ("http_proxy", PROXY_URL),
    ("NO_PROXY", "localhost,127.0.0.1"),
    ("no_proxy", "localhost,127.0.0.1"),
];

/// The usage line appended to every approved target egress.
pub(super) fn usage() -> String {
    format!("exec and watch commands get HTTPS_PROXY/HTTP_PROXY={PROXY_URL}, so npm ci, pip, cargo, git and curl reach every approved host unchanged. Restart a watch that started before the approval.")
}

/// Whether `service`'s current generation holds an approved target egress.
fn has_egress(state: &State, service: &Service) -> bool {
    state.ports.iter().any(|p| {
        p.service == service.resource.id
            && p.generation == service.resource.generation
            && p.direction == Direction::Egress
            && p.target.is_some()
            && p.status == Status::Approved
    })
}
/// Environment for a command started in `service`: the proxy settings when it
/// has approved egress, otherwise nothing.
pub(crate) fn env(state: &State, service: &Service) -> &'static [(&'static str, &'static str)] {
    if has_egress(state, service) { &ENV } else { &[] }
}
/// Shell `export` line equivalent to [`env`], for watch commands the runner
/// starts. Empty without egress.
pub(crate) fn exports(state: &State, service: &Service) -> String {
    let pairs = env(state, service);
    if pairs.is_empty() {
        return String::new();
    }
    format!("export {}\n", pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "))
}

/// The exact service instance one proxy serves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Key {
    service: ResourceId,
    generation: Generation,
    container: ContainerId,
    node: NodeId,
}
impl Key {
    pub(super) fn of(port: &Port) -> Self {
        Self { service: port.service.clone(), generation: port.generation, container: port.container.clone(), node: port.node.clone() }
    }
}
/// Live, authorized target egress grants of `key`.
fn grants<'a>(state: &'a State, key: &'a Key, time: u64) -> impl Iterator<Item = &'a Port> {
    state.ports.iter().filter(move |p| {
        p.direction == Direction::Egress
            && p.target.is_some()
            && Key::of(p) == *key
            && authorized(state, p, time).is_ok()
    })
}
/// The live grant for exactly `host:port`, if any.
fn grant(state: &State, key: &Key, host: &str, port: u16, time: u64) -> Option<Port> {
    grants(state, key, time)
        .find(|p| p.target.as_ref().is_some_and(|t| t.port.get() == port && t.host.eq_ignore_ascii_case(host)))
        .cloned()
}

struct Active {
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}
/// Running proxies, reconciled by the port supervisor on every pass.
#[derive(Default)]
pub(super) struct Proxies(Vec<(Key, Active)>);
impl Proxies {
    pub(super) fn sync(&mut self, shared: &Arc<Mutex<State>>, wanted: &[Key]) {
        for (_, active) in self.0.extract_if(.., |(key, active)| !wanted.contains(key) || active.handle.is_finished()) {
            active.cancel.store(true, Ordering::Release);
            let _ = active.handle.join();
        }
        for key in wanted {
            if self.0.iter().any(|(k, _)| k == key) {
                continue;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            let (shared, signal, id) = (shared.clone(), cancel.clone(), key.clone());
            let handle = thread::spawn(move || run(&shared, &id, &signal));
            self.0.push((key.clone(), Active { cancel, handle }));
        }
    }
    pub(super) fn stop(self) {
        for (_, active) in self.0 {
            active.cancel.store(true, Ordering::Release);
            let _ = active.handle.join();
        }
    }
}
fn run(shared: &Arc<Mutex<State>>, key: &Key, cancel: &Arc<AtomicBool>) {
    let alive = || {
        !cancel.load(Ordering::Acquire)
            && !crate::shutdown::requested()
            && shared.lock().is_ok_and(|state| grants(&state, key, now()).next().is_some())
    };
    while alive() {
        let _ = listen(shared, key, cancel, &alive);
        for _ in 0..10 {
            if !alive() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}
fn listen(shared: &Arc<Mutex<State>>, key: &Key, cancel: &Arc<AtomicBool>, alive: &dyn Fn() -> bool) -> io::Result<()> {
    let (service, expires) = {
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        let time = now();
        let ports = grants(&state, key, time).collect::<Vec<_>>();
        let first = ports.first().ok_or_else(|| fail("No approved egress"))?;
        let expires = ports.iter().filter_map(|p| p.approval.as_ref().map(|a| a.expires)).max().unwrap_or(time);
        (authorized(&state, first, time)?, expires)
    };
    let binding = crate::services::port_binding(&service, &key.container)?;
    let (shared, key, cancel) = (shared.clone(), key.clone(), cancel.clone());
    multiplex::serve(binding, PROXY_PORT, expires, alive, &|| {}, move |connection, link| {
        session(&shared, &key, &cancel, connection, link)
    })
}
/// Hands one relayed stream to [`handle`] through a private loopback pair, so
/// the request head can be read before any upstream exists.
fn session(shared: &Arc<Mutex<State>>, key: &Key, cancel: &AtomicBool, connection: Incoming, link: Arc<Transport>) {
    let Ok((client, relayed)) = pair() else {
        link.reject(connection);
        return;
    };
    if link.attach(connection, relayed).is_ok() {
        let _ = handle(shared, key, cancel, client);
    }
}
/// A connected loopback pair. Only our own connection is accepted.
fn pair() -> io::Result<(TcpStream, TcpStream)> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let near = TcpStream::connect(listener.local_addr()?)?;
    let local = near.local_addr()?;
    for _ in 0..4 {
        let (far, peer) = listener.accept()?;
        if peer == local {
            return Ok((near, far));
        }
    }
    Err(fail("Proxy pair was intercepted"))
}

/// A parsed proxy request.
#[derive(Debug, PartialEq, Eq)]
struct Request {
    host: String,
    port: u16,
    /// `None` for CONNECT; otherwise the head to send upstream, in origin form.
    forward: Option<Vec<u8>>,
}
fn read_head(client: &mut TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut data = Vec::new();
    let mut chunk = [0; 2048];
    loop {
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = data.split_off(end + 4);
            return Ok((data, rest));
        }
        if data.len() > HEAD_LIMIT {
            return Err(fail("Request head exceeds 8 KiB"));
        }
        match client.read(&mut chunk)? {
            0 => return Err(fail("Request ended before its head")),
            n => data.extend_from_slice(&chunk[..n]),
        }
    }
}
fn authority(text: &str, default: Option<u16>) -> Result<(String, u16), String> {
    let (host, port) = match text.strip_prefix('[') {
        Some(v6) => {
            let (host, rest) = v6.split_once(']').ok_or("Invalid IPv6 authority")?;
            (host, rest.strip_prefix(':'))
        }
        None => match text.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (text, None),
        },
    };
    let port = match port {
        Some(port) => port.parse::<u16>().map_err(|_| "Invalid port")?,
        None => default.ok_or("CONNECT needs host:port")?,
    };
    let endpoint = Endpoint { host: host.to_owned(), port: NonZeroU16::new(port).ok_or("Invalid port")? };
    endpoint.url().map_err(|e| e.to_string())?;
    Ok((endpoint.host, port))
}
fn parse(head: &[u8]) -> Result<Request, String> {
    let text = std::str::from_utf8(head).map_err(|_| "Request head is not UTF-8")?;
    let text = text.strip_suffix("\r\n\r\n").ok_or("Malformed request")?;
    let (line, headers) = text.split_once("\r\n").unwrap_or((text, ""));
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err("Malformed request line".into());
    };
    if !version.starts_with("HTTP/1.") {
        return Err("Only HTTP/1.x proxy requests are supported".into());
    }
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = authority(target, None)?;
        return Ok(Request { host, port, forward: None });
    }
    let scheme = target.get(..7).filter(|s| s.eq_ignore_ascii_case("http://")).ok_or(
        "Send CONNECT host:port for HTTPS, or an absolute http:// URI; this is Control's egress proxy",
    )?;
    let rest = &target[scheme.len()..];
    let (authority_text, path) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
    if authority_text.contains('@') {
        return Err("Credentials in proxy URIs are not supported".into());
    }
    let (host, port) = authority(authority_text, Some(80))?;
    let mut forward = format!("{method} {path} {version}\r\n").into_bytes();
    for header in headers.split("\r\n").filter(|h| !h.is_empty() && !h.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("proxy-"))) {
        forward.extend_from_slice(header.as_bytes());
        forward.extend_from_slice(b"\r\n");
    }
    forward.extend_from_slice(b"\r\n");
    Ok(Request { host, port, forward: Some(forward) })
}
fn respond(client: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    client.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
}
fn denied(host: &str, port: u16) -> String {
    format!("Control egress proxy: {host}:{port} is not an approved egress target for this service. Request an egress port for {host}:{port} with control_port_request (\"target\":{{\"host\":\"{host}\",\"port\":{port}}}) and wait for approval, then retry.\n")
}
/// Serves one proxy request read from `client`. Test seam: needs no relay.
pub(super) fn handle(shared: &Arc<Mutex<State>>, key: &Key, cancel: &AtomicBool, mut client: TcpStream) -> io::Result<()> {
    client.set_read_timeout(Some(HEAD_TIMEOUT))?;
    let (head, rest) = read_head(&mut client)?;
    let request = match parse(&head) {
        Ok(request) => request,
        Err(message) => return respond(&mut client, "400 Bad Request", &format!("{message}\n")),
    };
    let Request { host, port, forward } = request;
    if forward.is_some() && port != 80 {
        return respond(&mut client, "403 Forbidden", &format!("Control egress proxy relays plain HTTP only to port 80; use https:// (CONNECT) for {host}:{port}.\n"));
    }
    let live = |grant: &Port| {
        !cancel.load(Ordering::Acquire)
            && !crate::shutdown::requested()
            && shared.lock().is_ok_and(|state| authorized(&state, grant, now()).is_ok())
    };
    let grant = shared.lock().ok().and_then(|state| grant(&state, key, &host, port, now()));
    let Some((grant, target)) = grant.and_then(|g| g.approval.as_ref().and_then(|a| a.pinned).map(|t| (g, t))) else {
        return respond(&mut client, "403 Forbidden", &denied(&host, port));
    };
    let mut upstream = match TcpStream::connect_timeout(&target, Duration::from_secs(2)) {
        Ok(upstream) => upstream,
        Err(error) => return respond(&mut client, "502 Bad Gateway", &format!("Control egress proxy could not connect to {host}:{port}: {error}\n")),
    };
    if !live(&grant) {
        return respond(&mut client, "403 Forbidden", &denied(&host, port));
    }
    client.set_read_timeout(None)?;
    client.set_nodelay(true)?;
    upstream.set_nodelay(true)?;
    match forward {
        None => client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")?,
        Some(head) => upstream.write_all(&head)?,
    }
    upstream.write_all(&rest)?;
    pump(client, upstream, || live(&grant))
}
/// Copies both directions until each ends, or closes both once `live` fails.
fn pump(client: TcpStream, upstream: TcpStream, live: impl Fn() -> bool) -> io::Result<()> {
    let done = Arc::new(AtomicUsize::new(0));
    let copy = |mut from: TcpStream, mut to: TcpStream| {
        let done = done.clone();
        thread::spawn(move || {
            if io::copy(&mut from, &mut to).is_ok() {
                let _ = to.shutdown(Shutdown::Write);
            } else {
                let _ = from.shutdown(Shutdown::Both);
                let _ = to.shutdown(Shutdown::Both);
            }
            done.fetch_add(1, Ordering::AcqRel);
        })
    };
    let tasks = [copy(client.try_clone()?, upstream.try_clone()?), copy(upstream.try_clone()?, client.try_clone()?)];
    while done.load(Ordering::Acquire) < 2 && live() {
        thread::sleep(Duration::from_millis(250));
    }
    let _ = client.shutdown(Shutdown::Both);
    let _ = upstream.shutdown(Shutdown::Both);
    for task in tasks {
        let _ = task.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests;
