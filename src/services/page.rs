//! Operator view of persistent services: what runs, where, and how much it uses.
//! Engine queries run on a bounded snapshot outside the state lock, and only for
//! containers whose identity was verified first.
use super::*;
use std::{net::TcpStream, time::Instant};

const RECENT_STOPPED: usize = 5;
const PROCESSES: usize = 20;
const BUDGET: Duration = Duration::from_secs(3);

pub fn http(stream: &mut TcpStream, shared: &Arc<Mutex<State>>, method: &str, path: &str) -> bool {
    if path != "/tools/services" {
        return false;
    }
    let result = if method == "GET" { render(shared) } else { Err(fail("Use GET")) };
    let (status, page) = match result {
        Ok(page) => ("200 OK", page),
        Err(error) => ("400 Bad Request", crate::error_page(&error.to_string(), "/tools/services")),
    };
    let _ = control_http::respond(stream, status, "text/html; charset=utf-8", page.as_bytes(), "", control_http::Policy::Page);
    true
}

struct Live {
    stats: Option<crate::engine::Stats>,
    processes: Vec<crate::engine::Process>,
}

fn render(shared: &Arc<Mutex<State>>) -> io::Result<String> {
    let services = {
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        let active = state.services.iter().rev().filter(|s| s.resource.phase.reserves_capacity());
        let stopped = state.services.iter().rev().filter(|s| !s.resource.phase.reserves_capacity()).take(RECENT_STOPPED);
        active.chain(stopped).map(snapshot).collect::<Vec<_>>()
    };
    let live = observe(&services);
    let state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let cards = services.iter().map(|service| card(service, live.get(service.resource.id.as_str()))).collect::<String>();
    let empty = if services.is_empty() { "<p class='muted'>No persistent services yet. Agents start them from chat.</p>" } else { "" };
    Ok(crate::render(&state, "tool-page", "none", &format!("<h2>Persistent services</h2>{empty}{cards}")))
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

fn phase(phase: &Phase) -> &'static str {
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

fn card(service: &Service, live: Option<&Live>) -> String {
    let resource = &service.resource;
    let command = service.command.0.lines().next().unwrap_or_default().chars().take(160).collect::<String>();
    let lease = if resource.lease.is_until_stopped() {
        "until stopped".to_owned()
    } else {
        format!("lease ends in {} min", resource.lease.expires().saturating_sub(now()) / 60)
    };
    let storage = match service.storage {
        Storage::Memory => "memory storage".to_owned(),
        Storage::Disk { limit_mb: Some(limit) } => format!("disk storage, {limit} MB"),
        Storage::Disk { limit_mb: None } => "disk storage".to_owned(),
    };
    let chat = resource.owner.chat.as_str();
    let conversation = if crate::chat::valid_id(chat) { format!("<a href='/chat/{chat}'>Conversation</a> · ") } else { String::new() };
    let container = resource.phase.container().map(|id| format!("container {} · ", crate::escape(&id.as_str().chars().take(12).collect::<String>()))).unwrap_or_default();
    let image = resource.image.as_str().trim_start_matches("sha256:").chars().take(12).collect::<String>();
    let stats = live.and_then(|live| live.stats.as_ref()).map(|stats| {
        format!("<p>CPU {} · Memory {} · {} processes</p>", crate::escape(&stats.cpu), crate::escape(&stats.memory), crate::escape(&stats.pids))
    }).unwrap_or_default();
    let processes = live.filter(|live| !live.processes.is_empty()).map(|live| {
        let rows = live.processes.iter().map(|process| {
            format!("<tr><td>{}</td><td>{}</td><td><code>{}</code></td></tr>", crate::escape(&process.pid), crate::escape(&process.user), crate::escape(&process.command.chars().take(200).collect::<String>()))
        }).collect::<String>();
        format!("<table><thead><tr><th>PID</th><th>User</th><th>Command</th></tr></thead><tbody>{rows}</tbody></table>")
    }).unwrap_or_default();
    let diagnostic = if service.diagnostic.is_empty() { String::new() } else { format!("<p class='muted'>{}</p>", crate::escape(&service.diagnostic)) };
    format!(
        "<article class='record service-record'><h3><code>{}</code></h3><p>{} · {:?} profile · {storage} · {lease}</p><p>{conversation}{container}image {}</p>{stats}{processes}{diagnostic}</article>",
        crate::escape(&command),
        phase(&resource.phase),
        service.profile,
        crate::escape(&image),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_escape_commands_and_show_live_data_only_when_observed() {
        let (mut state, run, _chat, time) = super::super::tests::fixture();
        let input = Input { command: "<script>alert(1)</script>\nsecond line".to_owned().try_into().unwrap(), lease_seconds: -1, storage_limit_mb: None };
        admit(&mut state, Request { run: &run, generation: "attempt-a", nonce: "n1" }, input, "node-a".to_owned().try_into().unwrap(), time, |_| Ok(())).unwrap();
        let service = state.services[0].binding_snapshot();
        let plain = card(&service, None);
        assert!(plain.contains("&lt;script&gt;alert(1)&lt;/script&gt;") && !plain.contains("<script>"));
        assert!(!plain.contains("second line"));
        assert!(plain.contains("Requested") && plain.contains("until stopped") && plain.contains("/chat/"));
        assert!(!plain.contains("CPU") && !plain.contains("<table>"));
        let live = Live {
            stats: Some(crate::engine::Stats { cpu: "3.5%".into(), memory: "512MiB / 2GiB".into(), pids: "14".into() }),
            processes: vec![crate::engine::Process { pid: "77".into(), user: "502".into(), command: "java -jar server.jar <nogui>".into() }],
        };
        let observed = card(&service, Some(&live));
        assert!(observed.contains("CPU 3.5% · Memory 512MiB / 2GiB · 14 processes"));
        assert!(observed.contains("<td>77</td>") && observed.contains("java -jar server.jar &lt;nogui&gt;"));
    }

    #[test]
    fn page_snapshots_keep_escaped_diagnostics() {
        let (mut state, run, _chat, time) = super::super::tests::fixture();
        let input = Input { command: "sleep infinity".to_owned().try_into().unwrap(), lease_seconds: -1, storage_limit_mb: None };
        admit(&mut state, Request { run: &run, generation: "attempt-a", nonce: "n1" }, input, "node-a".to_owned().try_into().unwrap(), time, |_| Ok(())).unwrap();
        state.services[0].diagnostic = "Engine said <no>".into();
        assert!(state.services[0].binding_snapshot().diagnostic.is_empty());
        let shown = card(&snapshot(&state.services[0]), None);
        assert!(shown.contains("Engine said &lt;no&gt;"));
    }

    #[test]
    fn gather_keeps_results_that_arrive_before_the_deadline() {
        let jobs: Vec<(String, Job<u32>)> = vec![
            ("fast".into(), Box::new(|| 1)),
            ("slow".into(), Box::new(|| { std::thread::sleep(Duration::from_secs(2)); 2 })),
            ("also-fast".into(), Box::new(|| 3)),
        ];
        let started = Instant::now();
        let results = gather(jobs, Instant::now() + Duration::from_millis(300));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!((results.get("fast"), results.get("also-fast"), results.get("slow")), (Some(&1), Some(&3), None));
        assert!(gather::<u32>(Vec::new(), Instant::now() + Duration::from_secs(5)).is_empty());
    }
}
