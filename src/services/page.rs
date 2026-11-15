//! Operator view of persistent services: what runs, where, and how much it uses.
//! Engine queries run on a bounded snapshot outside the state lock, and only for
//! containers whose identity was verified first.
use super::*;
use std::{collections::HashSet, net::TcpStream, time::Instant};

const RECENT_STOPPED: usize = 5;
const PROCESSES: usize = 20;
const BUDGET: Duration = Duration::from_secs(3);

pub fn http(stream: &mut TcpStream, shared: &Arc<Mutex<State>>, method: &str, path: &str, body: &[u8]) -> bool {
    if path == "/tools/services/live" {
        let body = if method == "GET" { live(shared) } else { Err(fail("Use GET")) };
        let _ = match body {
            Ok(body) => control_http::respond(stream, "200 OK", "application/json", body.as_bytes(), "", control_http::Policy::Page),
            Err(error) => control_http::respond(stream, "400 Bad Request", "text/plain", error.to_string().as_bytes(), "", control_http::Policy::Page),
        };
        return true;
    }
    if path != "/tools/services" {
        return false;
    }
    let fields = crate::fields(&String::from_utf8_lossy(body));
    let back = fields.get("return_chat").filter(|id| crate::chat::valid_id(id)).map_or("/tools/services".to_owned(), |chat| format!("/chat/{chat}"));
    let result = match method {
        "GET" => render(shared).map(Some),
        "POST" if matches!(fields.get("action").map(String::as_str), Some("delete" | "cleanup")) => {
            let id = fields.get("id").filter(|_| fields.get("action").map(String::as_str) == Some("delete"));
            clean_up(shared, id.map(String::as_str)).map(|()| None)
        }
        "POST" => fields.get("id").ok_or_else(|| fail("Missing service ID")).and_then(|id| {
            let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
            stop(&mut state, id, now(), crate::storage::save)
        }).map(|()| None),
        _ => Err(fail("Use GET or POST")),
    };
    let _ = match result {
        Ok(Some(page)) => control_http::respond(stream, "200 OK", "text/html; charset=utf-8", page.as_bytes(), "", control_http::Policy::Client),
        Ok(None) => control_http::respond(stream, "303 See Other", "text/plain", b"", &format!("Location: {back}\r\n"), control_http::Policy::Page),
        Err(error) => control_http::respond(stream, "400 Bad Request", "text/html; charset=utf-8", crate::error_page(&error.to_string(), &back).as_bytes(), "", control_http::Policy::Page),
    };
    true
}

/// The operator's stop: the same transition an owning agent's stop makes. Ports
/// bound to the service lose authorization with it.
pub(crate) fn stop(state: &mut State, id: &str, time: u64, save: impl FnOnce(&State) -> io::Result<()>) -> io::Result<()> {
    let index = state.services.iter()
        .position(|service| service.resource.id.as_str() == id && service.resource.phase.reserves_capacity())
        .ok_or_else(|| fail("No active service with that ID"))?;
    if matches!(state.services[index].resource.phase, Phase::Stopping { .. }) {
        return Ok(());
    }
    transition(state, index, Event::Stop(StopReason::Requested), time, save).map(|_| ())
}

/// Removes stopped service records: one by ID, or all. Live services are never
/// touched. Returns the chats that still have services, whose workspaces stay,
/// and the deleted service's chat when one was named.
pub(crate) fn forget(state: &mut State, id: Option<&str>, save: impl FnOnce(&State) -> io::Result<()>) -> io::Result<(HashSet<String>, Option<String>)> {
    let removable = |service: &Service| !service.resource.phase.reserves_capacity() && id.is_none_or(|id| service.resource.id.as_str() == id);
    let chat = match id {
        Some(_) => Some(state.services.iter().find(|s| removable(s)).ok_or_else(|| fail("Only a stopped service can be deleted"))?.resource.owner.chat.as_str().to_owned()),
        None => None,
    };
    let previous = state.services.clone();
    state.services.retain(|service| !removable(service));
    if let Err(error) = save(state) {
        state.services = previous;
        return Err(error);
    }
    let keep = state.services.iter().map(|service| service.resource.owner.chat.as_str().to_owned()).collect();
    Ok((keep, chat))
}
/// Records change under the lock; engine volume removal runs after it is released.
fn clean_up(shared: &Arc<Mutex<State>>, id: Option<&str>) -> io::Result<()> {
    let (keep, chat) = forget(&mut *shared.lock().map_err(|_| fail("State unavailable"))?, id, crate::storage::save)?;
    let node = crate::config::node_id().map_err(fail)?;
    let (_, failures) = workspace::remove_unused(&node, &keep, chat.as_deref());
    if failures.is_empty() { Ok(()) } else { Err(fail(format!("Records were removed, but some workspaces remain: {}", failures.join("; ")))) }
}

struct Live {
    stats: Option<crate::engine::Stats>,
    processes: Vec<crate::engine::Process>,
}

/// The page renders from state at once; engine reads arrive through `live`,
/// which the web client fetches after the page is shown.
fn render(shared: &Arc<Mutex<State>>) -> io::Result<String> {
    let state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let services = listed(&state);
    let cards = services.iter().map(card).collect::<String>();
    let compute = compute_panel(&state.services);
    let empty = if services.is_empty() { "<p class='muted'>No persistent services yet. Agents start them from chat.</p>" } else { "" };
    let stopped = state.services.iter().filter(|s| !s.resource.phase.reserves_capacity()).count();
    let cleanup = if stopped == 0 { String::new() } else {
        format!("<form class='decision-form' method='post' action='/tools/services'><input type='hidden' name='action' value='cleanup'><button>Clean up {stopped} stopped service{}</button><span class='muted'> Removes their records and each chat's retained /workspace once no service of that chat remains.</span></form>", if stopped == 1 { "" } else { "s" })
    };
    let page = crate::render(&state, "tool-page", "none", &format!("<h2>Persistent services</h2>{compute}{cleanup}<div data-live-src='/tools/services/live'>{empty}{cards}</div>"));
    Ok(page.replacen("</head>", "<script type='module' src='/tool-window/chat-bootstrap.js'></script></head>", 1))
}
fn listed(state: &State) -> Vec<Service> {
    let active = state.services.iter().rev().filter(|s| s.resource.phase.reserves_capacity());
    let stopped = state.services.iter().rev().filter(|s| !s.resource.phase.reserves_capacity()).take(RECENT_STOPPED);
    active.chain(stopped).map(snapshot).collect()
}
/// Live usage per service ID as HTML fragments, read outside the state lock.
fn live(shared: &Arc<Mutex<State>>) -> io::Result<String> {
    let services = listed(&*shared.lock().map_err(|_| fail("State unavailable"))?);
    let fragments = observe(&services).iter().map(|(id, live)| (id.clone(), Value::String(usage(live)))).collect::<serde_json::Map<_, _>>();
    Ok(Value::Object(fragments).to_string())
}

/// The binding snapshot plus the diagnostic the page shows.
fn snapshot(service: &Service) -> Service {
    let mut snapshot = service.binding_snapshot();
    snapshot.diagnostic.clone_from(&service.diagnostic);
    snapshot
}

/// Verified running containers only. Inventory, then stats and process listings
/// in parallel, share one budget; failed or late engine reads leave a card
/// without live data.
fn observe(services: &[Service]) -> HashMap<String, Live> {
    let Ok(node) = crate::config::node_id() else { return HashMap::new() };
    let running = services.iter().filter(|s| matches!(s.resource.phase, Phase::Running(_))).collect::<Vec<_>>();
    if running.is_empty() {
        return HashMap::new();
    }
    let deadline = Instant::now() + BUDGET;
    let job: Job<io::Result<HashMap<String, Inspection>>> = Box::new(move || Docker.inventory(&node));
    let Some(Ok(inventory)) = gather(vec![(String::new(), job)], deadline).remove("") else { return HashMap::new() };
    let verified = running
        .into_iter()
        .filter_map(|service| {
            let id = service.resource.phase.container()?;
            verify(service, inventory.get(id.as_str())?).ok()?;
            Some((service.resource.id.as_str().to_owned(), id.as_str().to_owned()))
        })
        .collect::<Vec<_>>();
    enum Read {
        Stats(HashMap<String, crate::engine::Stats>),
        Processes(Vec<crate::engine::Process>),
    }
    let ids = verified.iter().map(|(_, container)| container.clone()).collect::<Vec<_>>();
    let mut jobs: Vec<(String, Job<Read>)> = vec![(String::new(), Box::new(move || {
        let ids = ids.iter().map(String::as_str).collect::<Vec<_>>();
        Read::Stats(crate::engine::stats(&ids).unwrap_or_default())
    }))];
    for (service, container) in &verified {
        let container = container.clone();
        jobs.push((service.clone(), Box::new(move || Read::Processes(crate::engine::top(&container, PROCESSES).unwrap_or_default()))));
    }
    let mut reads = gather(jobs, deadline);
    let mut stats = match reads.remove("") { Some(Read::Stats(stats)) => stats, _ => HashMap::new() };
    verified
        .into_iter()
        .map(|(service, container)| {
            let processes = match reads.remove(&service) { Some(Read::Processes(processes)) => processes, _ => Vec::new() };
            (service, Live { stats: stats.remove(&container), processes })
        })
        .collect()
}

type Job<T> = Box<dyn FnOnce() -> T + Send>;
/// Runs every job on its own thread and keeps the results that arrive before the
/// deadline. Late jobs finish in the background within their own engine timeout.
fn gather<T: Send + 'static>(jobs: Vec<(String, Job<T>)>, deadline: Instant) -> HashMap<String, T> {
    let (sender, results) = std::sync::mpsc::channel();
    for (key, job) in jobs {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let _ = sender.send((key, job()));
        });
    }
    drop(sender);
    let mut collected = HashMap::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match results.recv_timeout(left) {
            Ok((key, value)) => {
                collected.insert(key, value);
            }
            Err(_) => break,
        }
    }
    collected
}

pub(super) fn phase(phase: &Phase) -> &'static str {
    match phase {
        Phase::Requested => "Requested",
        Phase::Creating => "Creating",
        Phase::Starting(_) => "Starting",
        Phase::Running(_) => "Running",
        Phase::Stopping { .. } => "Stopping",
        Phase::Stopped(_) => "Stopped",
        Phase::Fenced { .. } => "Fenced",
    }
}

fn usage(live: &Live) -> String {
    let stats = live.stats.as_ref().map(|stats| {
        format!("<p>CPU {} · Memory {} · {} processes</p>", crate::escape(&stats.cpu), crate::escape(&stats.memory), crate::escape(&stats.pids))
    }).unwrap_or_default();
    if live.processes.is_empty() {
        return stats;
    }
    let rows = live.processes.iter().map(|process| {
        format!("<tr><td>{}</td><td>{}</td><td><code>{}</code></td></tr>", crate::escape(&process.pid), crate::escape(&process.user), crate::escape(&process.command.chars().take(200).collect::<String>()))
    }).collect::<String>();
    format!("{stats}<table><thead><tr><th>PID</th><th>User</th><th>Command</th></tr></thead><tbody>{rows}</tbody></table>")
}

fn compute_label(shape: Option<compute::Shape>, limits: compute::Limits) -> String {
    let memory = if limits.memory_bytes.is_multiple_of(compute::GIB) { format!("{} GiB", limits.memory_bytes / compute::GIB) } else { format!("{} MiB", limits.memory_bytes / (1024 * 1024)) };
    format!("{} · {memory} RAM · {} CPU{}", shape.map_or("default", compute::Shape::name), limits.cpus, if limits.cpus == 1 { "" } else { "s" })
}

fn compute_panel(services: &[Service]) -> String {
    let used = compute::reserved(services);
    let capacity = compute::capacity();
    let rows = compute::SHAPES.map(|shape| {
        let limits = shape.limits();
        format!("<tr><td><code>{}</code></td><td>{} GiB</td><td>{}</td></tr>", shape.name(), limits.memory_bytes / compute::GIB, limits.cpus)
    }).join("");
    format!("<section class='record service-record'><h3>Service resources</h3><p>{} MiB of {} GiB RAM · {} of {} CPU limits reserved ({} CPUs, {}x oversubscription)</p><table><thead><tr><th>Shape</th><th>Memory limit</th><th>CPU limit</th></tr></thead><tbody>{rows}</tbody></table><p class='muted'>Ask an agent to use a shape when starting a service. Defaults: Lite {} MiB, Full {} MiB; 1 CPU. To change size, stop and replace in the same chat; /workspace is retained.</p><p class='muted'>This budget covers Control services only. Allow headroom for models, workers and the host. Reserved limits are separate from live usage below.</p></section>", used.memory_bytes / (1024 * 1024), capacity.memory_bytes / compute::GIB, used.cpus, capacity.admitted_cpus(), capacity.cpus, capacity.cpu_oversubscription, crate::worker_profile::Profile::Lite.service_memory() / (1024 * 1024), crate::worker_profile::Profile::Full.service_memory() / (1024 * 1024))
}

fn card(service: &Service) -> String {
    let resource = &service.resource;
    let command = service.command.0.lines().next().unwrap_or_default().chars().take(160).collect::<String>();
    let lease = if resource.lease.is_until_stopped() {
        "until stopped".to_owned()
    } else {
        format!("lease ends in {} min", resource.lease.expires().saturating_sub(now()) / 60)
    };
    let mut storage = match service.storage {
        Storage::Memory => "memory storage".to_owned(),
        Storage::Disk { limit_mb: Some(limit) } | Storage::Workspace { limit_mb: Some(limit) } => format!("disk layer, {limit} MB"),
        Storage::Disk { limit_mb: None } | Storage::Workspace { limit_mb: None } => "disk storage".to_owned(),
    };
    if matches!(service.storage, Storage::Workspace { .. }) { storage.push_str(" · /workspace retained for this chat"); }
    let chat = resource.owner.chat.as_str();
    let conversation = if crate::chat::valid_id(chat) { format!("<a href='/chat/{chat}'>Conversation</a> · ") } else { String::new() };
    let container = resource.phase.container().map(|id| format!("container {} · ", crate::escape(&id.as_str().chars().take(12).collect::<String>()))).unwrap_or_default();
    let image = resource.image.as_str().trim_start_matches("sha256:").chars().take(12).collect::<String>();
    let live = if matches!(resource.phase, Phase::Running(_)) { format!("<div class='service-live' data-live='{}'><p class='muted'>Loading usage…</p></div>", crate::escape(resource.id.as_str())) } else { String::new() };
    let diagnostic = if service.diagnostic.is_empty() { String::new() } else { format!("<p class='muted'>{}</p>", crate::escape(&service.diagnostic)) };
    let action = if resource.phase.reserves_capacity() && !matches!(resource.phase, Phase::Stopping { .. }) {
        format!("<form class='decision-form' method='post' action='/tools/services'><input type='hidden' name='id' value='{}'><button>Stop service</button></form>", crate::escape(resource.id.as_str()))
    } else if !resource.phase.reserves_capacity() {
        format!("<form class='decision-form' method='post' action='/tools/services'><input type='hidden' name='action' value='delete'><input type='hidden' name='id' value='{}'><button>Delete</button></form>", crate::escape(resource.id.as_str()))
    } else {
        String::new()
    };
    let compute = compute_label(service.shape, service.limits());
    format!(
        "<article class='record service-record'><h3><code>{}</code></h3><p>{} · {compute}</p><p>{:?} image · {storage} · {lease}</p><p>{conversation}{container}image {}</p>{live}{diagnostic}{action}</article>",
        crate::escape(&command),
        phase(&resource.phase),
        service.profile,
        crate::escape(&image),
    )
}

#[cfg(test)]
mod tests;
