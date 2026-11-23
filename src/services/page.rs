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
        let body = if method == "GET" {
            live(shared)
        } else {
            Err(fail("Use GET"))
        };
        let _ = match body {
            Ok(body) => control_http::respond(
                stream,
                "200 OK",
                "application/json",
                body.as_bytes(),
                "",
                control_http::Policy::Page,
            ),
            Err(error) => control_http::respond(
                stream,
                "400 Bad Request",
                "text/plain",
                error.to_string().as_bytes(),
                "",
                control_http::Policy::Page,
            ),
        };
        return true;
    }
    if path != "/tools/services" {
        return false;
    }
    let fields = crate::fields(&String::from_utf8_lossy(body));
    let return_chat = fields.get("return_chat").filter(|id| crate::chat::valid_id(id));
    let back = return_chat.map_or("/tools/services".to_owned(), |chat| format!("/dm/{chat}"));
    // Per-card actions return to their card, not the top of the page.
    let place = match (method, return_chat, fields.get("id")) {
        ("POST", None, Some(id)) => shared.lock().map(|state| nearby(&state, id)).unwrap_or_default(),
        _ => Vec::new(),
    };
    let result = match method {
        "GET" => render(shared).map(Some),
        "POST" if matches!(fields.get("action").map(String::as_str), Some("delete" | "cleanup")) => {
            let id = fields
                .get("id")
                .filter(|_| fields.get("action").map(String::as_str) == Some("delete"));
            clean_up(shared, id.map(String::as_str)).map(|()| None)
        }
        "POST" => fields
            .get("id")
            .ok_or_else(|| fail("Missing service ID"))
            .and_then(|id| {
                let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
                stop(&mut state, id, now(), crate::storage::save)
            })
            .map(|()| None),
        _ => Err(fail("Use GET or POST")),
    };
    let _ = match result {
        Ok(Some(page)) => control_http::respond(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            page.as_bytes(),
            "",
            control_http::Policy::Client,
        ),
        Ok(None) => {
            let anchor = shared.lock().map(|state| fragment(&state, &place)).unwrap_or_default();
            control_http::respond(
                stream,
                "303 See Other",
                "text/plain",
                b"",
                &format!("Location: {back}{anchor}\r\n"),
                control_http::Policy::Page,
            )
        }
        Err(error) => control_http::respond(
            stream,
            "400 Bad Request",
            "text/html; charset=utf-8",
            crate::error_page(&error.to_string(), &back).as_bytes(),
            "",
            control_http::Policy::Page,
        ),
    };
    true
}

/// The operator's stop: the same transition an owning agent's stop makes. Ports
/// bound to the service lose authorization with it.
pub(crate) fn stop(
    state: &mut State,
    id: &str,
    time: u64,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> io::Result<()> {
    let index = state
        .services
        .iter()
        .position(|service| service.resource.id.as_str() == id && service.resource.phase.reserves_capacity())
        .ok_or_else(|| fail("No active service with that ID"))?;
    if matches!(state.services[index].resource.phase, Phase::Stopping { .. }) {
        return Ok(());
    }
    transition(state, index, Event::Stop(StopReason::Requested), time, save).map(|_| ())
}

/// `/services [stop ID|all]` in a chat: lists or stops the live services of the
/// chat's task tree (its top-level chat and every chat spawned beneath it), as
/// the services page would. Only services of the chat's actor; the operator
/// sees all of the tree's. IDs match exactly or by a unique prefix, with or
/// without `svc-`. Workspaces are always kept.
pub(crate) fn command(
    state: &mut State,
    chat: &str,
    option: &str,
    time: u64,
    save: impl Fn(&State) -> io::Result<()>,
) -> Result<String, &'static str> {
    let session = state
        .chats
        .iter()
        .find(|session| session.id == chat)
        .ok_or("Chat not found.")?;
    identity::authorize(&session.actor, Action::Task)?;
    let actor = session.actor.clone();
    let team = crate::chat::team(state, chat);
    let live = state
        .services
        .iter()
        .filter(|service| service.live() && team.contains(service.resource.owner.chat.as_str()))
        .filter(|service| actor == Actor::LocalOperator || service.actor == actor)
        .collect::<Vec<_>>();
    let mut words = option.split_whitespace();
    let ids = match (words.next(), words.next(), words.next()) {
        (None | Some("list"), None, _) => return Ok(team_list(state, chat, &live)),
        (Some("stop"), Some("all"), None) => live
            .iter()
            .filter(|service| !matches!(service.resource.phase, Phase::Stopping { .. }))
            .map(|service| service.resource.id.as_str().to_owned())
            .collect::<Vec<_>>(),
        (Some("stop"), Some(wanted), None) => {
            let wanted = wanted.strip_prefix("svc-").unwrap_or(wanted);
            let found = live
                .iter()
                .filter(|service| {
                    service
                        .resource
                        .id
                        .as_str()
                        .trim_start_matches("svc-")
                        .starts_with(wanted)
                })
                .collect::<Vec<_>>();
            match found.as_slice() {
                [service] => vec![service.resource.id.as_str().to_owned()],
                [] => return Err("No running service of this DM's team has that ID. Use /services to list them."),
                _ => return Err("That ID prefix matches several services. Use more of the ID shown by /services."),
            }
        }
        _ => return Err("Use /services, /services stop ID or /services stop all."),
    };
    if ids.is_empty() {
        return Ok("No running services to stop in this DM's team.".into());
    }
    for id in &ids {
        stop(state, id, time, &save).map_err(|_| "Could not save the stop. Try again.")?;
    }
    Ok(format!(
        "Stopping {}. Containers are removed; each DM's /workspace is kept. /services shows progress.",
        ids.join(", ")
    ))
}
fn team_list(state: &State, chat: &str, live: &[&Service]) -> String {
    if live.is_empty() {
        return "No running services in this DM's team. Stopped services free their capacity automatically; /tools/services lists recent ones.".into();
    }
    let rows = live
        .iter()
        .map(|service| {
            let owner = service.resource.owner.chat.as_str();
            let owner = if owner == chat {
                "this DM".to_owned()
            } else {
                crate::callsigns::name_or_id(&state.chats, owner).into_owned()
            };
            format!(
                "- {} · {} · {} · {owner} · {}",
                service.resource.id.as_str(),
                service.phase_name(),
                service.shape_name(),
                service.label()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Services of this DM's team ({}):\n{rows}\nStop one with /services stop ID (a unique prefix is enough) or all with /services stop all. /workspace volumes are kept.",
        live.len()
    )
}

/// Removes stopped service records before their workspaces are deleted: every
/// stopped record of the named stopped service's DM, or all stopped records.
/// Live services are never touched, and a DM with a live service keeps its
/// workspace in use, so naming one of its services is refused. Returns the
/// chats that still have services, whose workspaces stay, and the named DM.
/// Stopped records need no other clean-up: they hold no capacity and the
/// oldest are dropped automatically when records run out.
pub(crate) fn forget(
    state: &mut State,
    id: Option<&str>,
    save: impl FnOnce(&State) -> io::Result<()>,
) -> io::Result<(HashSet<String>, Option<String>)> {
    let chat = match id {
        Some(id) => {
            let chat = state
                .services
                .iter()
                .find(|s| !s.live() && s.resource.id.as_str() == id)
                .ok_or_else(|| fail("Only a stopped service's workspace can be deleted"))?
                .resource
                .owner
                .chat
                .as_str()
                .to_owned();
            if state
                .services
                .iter()
                .any(|s| s.live() && s.resource.owner.chat.as_str() == chat)
            {
                return Err(fail(
                    "This DM still has a running service using its /workspace; stop it first",
                ));
            }
            Some(chat)
        }
        None => None,
    };
    let removable = |service: &Service| {
        !service.live()
            && chat
                .as_deref()
                .is_none_or(|chat| service.resource.owner.chat.as_str() == chat)
    };
    let previous = state.services.clone();
    state.services.retain(|service| !removable(service));
    if let Err(error) = save(state) {
        state.services = previous;
        return Err(error);
    }
    let keep = state
        .services
        .iter()
        .map(|service| service.resource.owner.chat.as_str().to_owned())
        .collect();
    Ok((keep, chat))
}
/// Records change under the lock; engine volume removal runs after it is released.
fn clean_up(shared: &Arc<Mutex<State>>, id: Option<&str>) -> io::Result<()> {
    let (keep, chat) = forget(
        &mut *shared.lock().map_err(|_| fail("State unavailable"))?,
        id,
        crate::storage::save,
    )?;
    let node = crate::config::node_id().map_err(fail)?;
    let (_, failures) = workspace::remove_unused(crate::engine::isolation(), &node, &keep, chat.as_deref());
    if failures.is_empty() {
        Ok(())
    } else {
        Err(fail(format!(
            "Stopped records were removed, but some workspaces remain: {}",
            failures.join("; ")
        )))
    }
}
const STOP_CONFIRM: &str =
    "Stop this service? Its container and any files outside /workspace are removed. The DM's /workspace is kept.";
const DELETE_CONFIRM: &str = "Delete this DM's retained /workspace? Its files are lost and cannot be recovered.";
const CLEANUP_CONFIRM: &str =
    "Delete the retained /workspace of every DM with no running service? Their files are lost and cannot be recovered.";

struct Live {
    stats: Option<control_isolation::Stats>,
    processes: Vec<control_isolation::Process>,
}

/// The page renders from state at once; engine reads arrive through `live`,
/// which the web client fetches after the page is shown. Everything derived
/// from state sits inside the `data-live-refresh` region, which the client
/// re-reads from this page on each state-change hint (a stop that finishes,
/// a new service), so a finished stop moves its card to the stopped list
/// without a reload. Stopped services need no clean-up step: they leave the
/// cards, hold no capacity, and old records are dropped automatically.
fn render(shared: &Arc<Mutex<State>>) -> io::Result<String> {
    let state = shared.lock().map_err(|_| fail("State unavailable"))?;
    let services = listed(&state);
    let cards = services.iter().filter(|s| s.live()).map(card).collect::<String>();
    let compute = compute_panel(&state.services);
    let empty = if cards.is_empty() {
        "<p class='muted'>No running services. Agents start them from a DM.</p>"
    } else {
        ""
    };
    let busy = state
        .services
        .iter()
        .filter(|s| s.live())
        .map(|s| s.resource.owner.chat.as_str())
        .collect::<HashSet<_>>();
    let stopped = stopped_list(&services, &busy);
    let page = crate::render(
        &state,
        "tool-page",
        "none",
        &format!(
            "<h2>Persistent services</h2><div data-live-src='/tools/services/live' data-live-refresh>{compute}{empty}{cards}{stopped}</div>"
        ),
    );
    Ok(crate::web_assets::with_client(&page))
}
/// Recent stopped services as compact rows. A DM's retained /workspace can be
/// deleted from its newest stopped row once none of its services runs.
fn stopped_list(services: &[Service], busy: &HashSet<&str>) -> String {
    let mut offered = HashSet::new();
    let rows = services.iter().filter(|s| !s.live()).map(|service| {
        let resource = &service.resource;
        let chat = resource.owner.chat.as_str();
        let link = if crate::chat::valid_id(chat) { format!(" · <a href='/dm/{chat}'>Conversation</a>") } else { String::new() };
        let diagnostic = if service.diagnostic.is_empty() { String::new() } else { format!(" · {}", crate::escape(&service.diagnostic)) };
        let delete = if workspace::Workspace::of(service).is_some() && !busy.contains(chat) && offered.insert(chat) {
            format!("<form class='decision-form' method='post' action='/tools/services' data-confirm=\"{DELETE_CONFIRM}\"><input type='hidden' name='action' value='delete'><input type='hidden' name='id' value='{}'><button>Delete /workspace</button></form>", crate::escape(resource.id.as_str()))
        } else { String::new() };
        format!("<li id='{}'><code>{}</code> <span class='muted'>· {} · {}{link}{diagnostic}</span>{delete}</li>",
            crate::escape(resource.id.as_str()), crate::escape(&service.label()), phase(&resource.phase), crate::escape(resource.id.as_str()))
    }).collect::<String>();
    if rows.is_empty() {
        return String::new();
    }
    format!(
        "<section class='record service-record service-stopped'><h3>Recently stopped</h3><p class='muted'>Stopped services hold no capacity and need no clean-up; older records are dropped automatically. Each DM's /workspace is kept for its next service until you delete it.</p><ul>{rows}</ul><form class='decision-form' method='post' action='/tools/services' data-confirm=\"{CLEANUP_CONFIRM}\"><input type='hidden' name='action' value='cleanup'><button>Delete unused workspaces</button><span class='muted'> Every DM with no running service loses its /workspace.</span></form></section>"
    )
}
fn listed(state: &State) -> Vec<Service> {
    shown(state).map(snapshot).collect()
}
/// The services the page shows as cards, in page order.
fn shown(state: &State) -> impl Iterator<Item = &Service> {
    let active = state
        .services
        .iter()
        .rev()
        .filter(|s| s.resource.phase.reserves_capacity());
    let stopped = state
        .services
        .iter()
        .rev()
        .filter(|s| !s.resource.phase.reserves_capacity())
        .take(RECENT_STOPPED);
    active.chain(stopped)
}
/// Card IDs nearest the acted-on one: itself, then later cards, then earlier
/// ones. Taken before an action so its redirect can find the same place even
/// when the card is removed or leaves the page. IDs come from state only.
fn nearby(state: &State, id: &str) -> Vec<String> {
    let ids = shown(state)
        .map(|service| service.resource.id.as_str())
        .collect::<Vec<_>>();
    let Some(index) = ids.iter().position(|shown| *shown == id) else {
        return Vec::new();
    };
    ids[index..]
        .iter()
        .chain(ids[..index].iter().rev())
        .map(|id| (*id).to_owned())
        .collect()
}
/// The URL fragment of the first of `nearby` still shown, or none (page top).
fn fragment(state: &State, nearby: &[String]) -> String {
    let ids = shown(state)
        .map(|service| service.resource.id.as_str())
        .collect::<HashSet<_>>();
    nearby
        .iter()
        .find(|id| ids.contains(id.as_str()))
        .map(|id| format!("#{id}"))
        .unwrap_or_default()
}
/// Live usage per service ID as HTML fragments, read outside the state lock.
fn live(shared: &Arc<Mutex<State>>) -> io::Result<String> {
    let services = listed(&*shared.lock().map_err(|_| fail("State unavailable"))?);
    let fragments = observe(&services)
        .iter()
        .map(|(id, live)| (id.clone(), Value::String(usage(live))))
        .collect::<serde_json::Map<_, _>>();
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
    let Ok(node) = crate::config::node_id() else {
        return HashMap::new();
    };
    let running = services
        .iter()
        .filter(|s| matches!(s.resource.phase, Phase::Running(_)))
        .collect::<Vec<_>>();
    if running.is_empty() {
        return HashMap::new();
    }
    let deadline = Instant::now() + BUDGET;
    let isolation = crate::engine::isolation();
    let job: Job<io::Result<HashMap<String, Inspection>>> = Box::new(move || inventory(isolation, &node));
    let Some(Ok(inventory)) = gather(vec![(String::new(), job)], deadline).remove("") else {
        return HashMap::new();
    };
    let verified = running
        .into_iter()
        .filter_map(|service| {
            let id = service.resource.phase.container()?;
            verify(service, inventory.get(id.as_str())?).ok()?;
            Some((service.resource.id.as_str().to_owned(), id.as_str().to_owned()))
        })
        .collect::<Vec<_>>();
    enum Read {
        Stats(HashMap<String, control_isolation::Stats>),
        Processes(Vec<control_isolation::Process>),
    }
    let ids = verified
        .iter()
        .map(|(_, container)| container.clone())
        .collect::<Vec<_>>();
    let mut jobs: Vec<(String, Job<Read>)> = vec![(
        String::new(),
        Box::new(move || {
            let ids = ids
                .iter()
                .map(|id| control_isolation::ContainerId::from(id.as_str()))
                .collect::<Vec<_>>();
            Read::Stats(isolation.stats(&ids.iter().collect::<Vec<_>>()).unwrap_or_default())
        }),
    )];
    for (service, container) in &verified {
        let container = container.clone();
        jobs.push((
            service.clone(),
            Box::new(move || {
                Read::Processes(
                    isolation
                        .top(&container.as_str().into())
                        .map(|top| top.into_iter().take(PROCESSES).collect())
                        .unwrap_or_default(),
                )
            }),
        ));
    }
    let mut reads = gather(jobs, deadline);
    let mut stats = match reads.remove("") {
        Some(Read::Stats(stats)) => stats,
        _ => HashMap::new(),
    };
    verified
        .into_iter()
        .map(|(service, container)| {
            let processes = match reads.remove(&service) {
                Some(Read::Processes(processes)) => processes,
                _ => Vec::new(),
            };
            (
                service,
                Live {
                    stats: stats.remove(&container),
                    processes,
                },
            )
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
        Phase::Stopped(StopReason::Failed) => "Failed",
        Phase::Stopped(_) => "Stopped",
        Phase::Fenced { .. } => "Fenced",
    }
}

fn usage(live: &Live) -> String {
    let stats = live
        .stats
        .as_ref()
        .map(|stats| {
            format!(
                "<p><span class='muted'>Live usage</span> · CPU {} · Memory {} · {} processes</p>",
                crate::escape(&stats.cpu),
                crate::escape(&stats.memory),
                crate::escape(&stats.pids)
            )
        })
        .unwrap_or_default();
    if live.processes.is_empty() {
        return stats;
    }
    let rows = live
        .processes
        .iter()
        .map(|process| {
            format!(
                "<tr><td>{}</td><td>{}</td><td><code>{}</code></td></tr>",
                crate::escape(&process.pid),
                crate::escape(&process.user),
                crate::escape(&process.command.chars().take(200).collect::<String>())
            )
        })
        .collect::<String>();
    format!(
        "{stats}<table><thead><tr><th>PID</th><th>User</th><th>Command</th></tr></thead><tbody>{rows}</tbody></table>"
    )
}

fn memory_label(bytes: u64) -> String {
    if bytes.is_multiple_of(compute::GIB) {
        format!("{} GiB", bytes / compute::GIB)
    } else {
        format!("{} MiB", bytes / (1024 * 1024))
    }
}

/// RAM, CPU and process limits without the size name.
fn limits_label(limits: compute::Limits, pids: u64) -> String {
    format!(
        "{} RAM · {} CPU{} · {pids} processes",
        memory_label(limits.memory_bytes),
        limits.cpus,
        if limits.cpus == 1 { "" } else { "s" }
    )
}

fn compute_panel(services: &[Service]) -> String {
    use crate::worker_profile::Profile;
    let used = compute::reserved(services);
    let capacity = compute::capacity();
    let images = [Profile::Lite, Profile::Full]
        .map(|profile| {
            let limits = compute::limits(None, profile);
            format!(
                "<tr><td><strong>{}</strong></td><td>{}</td><td>{}</td></tr>",
                profile.label(),
                profile.toolset(),
                limits_label(limits, compute::pids_limit(limits.memory_bytes, profile, capacity))
            )
        })
        .join("");
    let sizes = compute::SHAPES
        .map(|shape| {
            let limits = shape.limits();
            format!(
                "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
                shape.name(),
                memory_label(limits.memory_bytes),
                limits.cpus,
                compute::pids_limit(limits.memory_bytes, Profile::Lite, capacity)
            )
        })
        .join("");
    format!(
        "<section class='record service-record'><h3>Service resources</h3><p class='muted'>Each service has an image and a size, chosen separately: any image runs at any size. Ask an agent for a size when it starts a service; bots can share services or run without one.</p><h4>Image · which tools</h4><table><thead><tr><th>Image</th><th>Tools</th><th>Default size</th></tr></thead><tbody>{images}</tbody></table><p class='muted'>The node's pinned worker image decides Lite or Full. Its default size applies only when no size is chosen.</p><h4>Size · how much it may use</h4><table><thead><tr><th>Size</th><th>RAM limit</th><th>CPU limit</th><th>Process limit</th></tr></thead><tbody>{sizes}</tbody></table><p class='muted'>Process limits (threads included) scale at {} per GiB of RAM, up to {}, never below the image default (Lite {}, Full {}). Each service keeps the limits captured when it started.</p><h4>Total service budget</h4><p><strong>RAM limits reserved:</strong> {} of {}<br><strong>CPU limits reserved:</strong> {} of {} CPUs</p><p class='muted'>Totals add the full limits of active services, including idle ones. Containers consume RAM as needed up to their limit. The CPU budget allows {} configured CPUs × {}.</p><p class='muted'>This service budget excludes model inference, model workers and the host. Leave capacity for those separately.</p><p class='muted'>Actual consumption appears as <strong>Live usage</strong> on each running service below. To resize, stop and replace the service in the same DM; its <code>/workspace</code> is retained.</p></section>",
        capacity.pids_per_gib,
        capacity.pids_max,
        Profile::Lite.service_pids(),
        Profile::Full.service_pids(),
        memory_label(used.memory_bytes),
        memory_label(capacity.memory_bytes),
        used.cpus,
        capacity.admitted_cpus(),
        capacity.cpus,
        capacity.cpu_oversubscription
    )
}

fn card(service: &Service) -> String {
    let resource = &service.resource;
    let command = service
        .command
        .0
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(160)
        .collect::<String>();
    let lease = if resource.lease.is_until_stopped() {
        "until stopped".to_owned()
    } else {
        format!(
            "lease ends in {} min",
            resource.lease.expires().saturating_sub(now()) / 60
        )
    };
    let mut storage = match service.storage {
        Storage::Memory => "memory storage".to_owned(),
        Storage::Disk { limit_mb: Some(limit) } | Storage::Workspace { limit_mb: Some(limit) } => {
            format!("disk layer, {limit} MB")
        }
        Storage::Disk { limit_mb: None } | Storage::Workspace { limit_mb: None } => "disk storage".to_owned(),
    };
    if matches!(service.storage, Storage::Workspace { .. }) {
        storage.push_str(" · /workspace retained for this chat");
    }
    let chat = resource.owner.chat.as_str();
    let links = [
        crate::chat::valid_id(chat).then(|| format!("<a href='/dm/{chat}'>Conversation</a>")),
        resource.phase.container().map(|id| {
            format!(
                "container {}",
                crate::escape(&id.as_str().chars().take(12).collect::<String>())
            )
        }),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let links = if links.is_empty() {
        String::new()
    } else {
        format!("<p>{}</p>", links.join(" · "))
    };
    let image = resource
        .image
        .as_str()
        .trim_start_matches("sha256:")
        .chars()
        .take(12)
        .collect::<String>();
    let live = if matches!(resource.phase, Phase::Running(_)) {
        format!(
            "<div class='service-live' data-live='{}'><p class='muted'>Live usage loading…</p></div>",
            crate::escape(resource.id.as_str())
        )
    } else {
        String::new()
    };
    let diagnostic = if service.diagnostic.is_empty() {
        String::new()
    } else {
        format!("<p class='muted'>{}</p>", crate::escape(&service.diagnostic))
    };
    // Stopped services are listed by `stopped_list`, not as cards.
    let action = if resource.phase.reserves_capacity() && !matches!(resource.phase, Phase::Stopping { .. }) {
        format!(
            "<form class='decision-form' method='post' action='/tools/services' data-confirm=\"{STOP_CONFIRM}\"><input type='hidden' name='id' value='{}'><button>Stop service</button></form>",
            crate::escape(resource.id.as_str())
        )
    } else {
        String::new()
    };
    // Image (tools) and size (limits) are independent choices, so each gets its own row.
    let size = service.shape.map_or("image default", compute::Shape::name);
    let limits = limits_label(service.limits(), service.pids_limit());
    format!(
        "<article class='record service-record' id='{}'><h3><code>{}</code></h3><dl class='service-facts'><dt>Status</dt><dd>{}</dd><dt>Image</dt><dd><strong>{}</strong> <span class='muted'>· {} · <code>{}</code></span></dd><dt>Size</dt><dd><strong>{size}</strong> <span class='muted'>· {limits}</span></dd><dt>Storage</dt><dd>{storage} · {lease}</dd></dl>{links}{live}{diagnostic}{action}</article>",
        crate::escape(resource.id.as_str()),
        crate::escape(&command),
        phase(&resource.phase),
        service.profile.label(),
        service.profile.toolset(),
        crate::escape(&image),
    )
}

#[cfg(test)]
mod tests;
