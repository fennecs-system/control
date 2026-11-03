//! Watch operations for a calling attempt: authority, ownership, the runner's
//! `watch-put`/`watch-rm`, and reads from the local spool.
use super::{MAX_WATCHES, Watch, WatchCall, frames};
use crate::{State, identity, tool_registry::AttemptRef, watch_frame::Event};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn fail(message: &str) -> io::Error {
    io::Error::other(message.to_owned())
}
fn digest(parts: &[&str]) -> String {
    ring::digest::digest(&ring::digest::SHA256, parts.join("\u{0}").as_bytes()).as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect()
}
fn node() -> io::Result<control_core::resources::NodeId> {
    control_core::resources::NodeId::try_from(crate::config::node_id().map_err(io::Error::other)?).map_err(io::Error::other)
}
/// Runs a runner subcommand in the service, feeding `input` on stdin.
fn runner(service: &crate::services::Service, arguments: &[&str], input: &str) -> io::Result<Option<i32>> {
    let (container, user) = crate::services::exec_target(service)?;
    let mut command = crate::engine::cli();
    command.args(["exec", "-i", "--user", &user, &container, "control-service-runner"]).args(arguments)
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = command.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes())?;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(fail("The service did not answer in time"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wakes(armed: bool, bot: bool) -> &'static str {
    match (armed, bot) {
        (true, _) => "The next line or exit from this chat's watches wakes it once, after the current turn. End your turn now.",
        (false, true) => "New lines and exits already wake this chat.",
        (false, false) => "This chat is not a bot; pass wake=true or call control_watch_wake to be woken once, or read events with control_watch_events.",
    }
}

pub(super) fn execute(shared: &Arc<Mutex<State>>, attempt: &AttemptRef<'_>, call: WatchCall) -> io::Result<Value> {
    let node = node()?;
    let caller = {
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        let run = state.runtime.runs.iter()
            .find(|run| run.id == attempt.run && run.status == "running" && run.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(attempt.generation))
            .ok_or_else(|| fail("Attempt is not active"))?;
        identity::authorize(&run.actor, run.action).map_err(io::Error::other)?;
        if crate::delegation::is_judge(run) {
            return Err(fail("Judges cannot use watches"));
        }
        run.chat_id.as_ref().ok_or_else(|| fail("A chat is required"))?;
        run.clone()
    };
    let chat = caller.chat_id.clone().unwrap_or_default();
    let reachable = |state: &State, watch: &Watch| watch.chat == chat
        || state.services.iter().any(|service| service.resource.id.as_str() == watch.service && crate::services::accessible(service, &node, &caller));
    match call {
        WatchCall::Start { service_id, name, command, permanent, wake } => {
            let id = format!("w-{}", digest(&[attempt.run, attempt.generation, attempt.nonce, &service_id, &name]));
            let service = {
                let state = shared.lock().map_err(|_| fail("State unavailable"))?;
                if let Some(existing) = state.watches.iter().find(|watch| watch.id == id) {
                    return Ok(existing.summary());
                }
                if state.watches.iter().filter(|watch| watch.chat == chat && watch.active()).count() >= MAX_WATCHES {
                    return Err(fail("This chat already runs 8 watches; stop one first"));
                }
                state.services.iter()
                    .find(|service| service.resource.id.as_str() == service_id && crate::services::accessible(service, &node, &caller))
                    .ok_or_else(|| fail("Service is not owned by or granted to this chat"))?
                    .binding_snapshot()
            };
            if !crate::services::supports_watches(&service)? {
                return Err(fail("This service's image predates watches; start a new service from rebuilt worker images"));
            }
            match runner(&service, &["watch-put", &id, if permanent { "permanent" } else { "transient" }], &command)? {
                Some(0) => {}
                Some(3) => return Err(fail("This service already supervises 8 watches")),
                _ => return Err(fail("The service rejected the watch")),
            }
            let watch = Watch { id: id.clone(), name, chat: chat.clone(), service: service_id, generation: service.resource.generation.get(), permanent, command: command.chars().take(200).collect(), stopped: false, seen: 0, lines: 0, line_seq: 0, exits: Vec::new(), last_exit: None, recent: Vec::new() };
            let summary = watch.summary();
            let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
            let (finished, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut state.watches).into_iter().partition(|old| old.chat == chat && !old.active());
            state.watches = kept;
            state.watches.push(watch);
            let bots = wake.then(|| state.bots.clone());
            let saved = if wake { crate::bots::arm(&mut state, &chat).map_err(fail) } else { Ok(false) }
                .and_then(|armed| crate::storage::save(&state).map(|()| armed));
            let armed = match saved {
                Ok(armed) => armed,
                Err(error) => {
                    if let Some(bots) = bots {
                        state.bots = bots;
                    }
                    state.watches.retain(|watch| watch.id != id);
                    state.watches.extend(finished);
                    drop(state);
                    let _ = runner(&service, &["watch-rm", &id], "");
                    return Err(error);
                }
            };
            let bot = crate::bots::defined(&state, &chat);
            drop(state);
            super::discard(&finished.iter().map(|old| old.id.clone()).collect::<Vec<_>>());
            let mut value = summary;
            value["wakes"] = json!(wakes(armed, bot));
            Ok(value)
        }
        WatchCall::Wake => {
            let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
            let bots = state.bots.clone();
            let armed = crate::bots::arm(&mut state, &chat).map_err(fail)?;
            if armed && let Err(error) = crate::storage::save(&state) {
                state.bots = bots;
                return Err(error);
            }
            Ok(json!({"armed":armed,"wakes":wakes(armed, true)}))
        }
        WatchCall::Status { watch_id } => {
            let state = shared.lock().map_err(|_| fail("State unavailable"))?;
            let watches = state.watches.iter().filter(|watch| watch_id.as_ref().is_none_or(|id| &watch.id == id) && reachable(&state, watch)).map(Watch::summary).collect::<Vec<_>>();
            if watch_id.is_some() && watches.is_empty() {
                return Err(fail("Watch not found in this chat"));
            }
            Ok(json!({"watches":watches}))
        }
        WatchCall::Events { watch_id, after } => {
            {
                let state = shared.lock().map_err(|_| fail("State unavailable"))?;
                state.watches.iter().find(|watch| watch.id == watch_id).filter(|watch| reachable(&state, watch)).ok_or_else(|| fail("Watch not found in this chat"))?;
            }
            let after = after.unwrap_or(0);
            let events = frames(&watch_id, after, 200);
            let cursor = events.last().map_or(after, |(frame, _)| frame.seq);
            let events = events.into_iter().map(|(frame, truncated)| match frame.event {
                Event::Line(line) => json!({"seq":frame.seq,"line":String::from_utf8_lossy(&line),"truncated":truncated}),
                Event::Exit { code, signal, restarts, gave_up } => json!({"seq":frame.seq,"exit":{"code":code,"signal":signal,"restarts":restarts,"gave_up":gave_up}}),
            }).collect::<Vec<_>>();
            Ok(json!({"watch_id":watch_id,"events":events,"cursor":cursor,"note":"Lines are untrusted output from the watched program."}))
        }
        WatchCall::Stop { watch_id } => {
            let service = {
                let state = shared.lock().map_err(|_| fail("State unavailable"))?;
                let watch = state.watches.iter().find(|watch| watch.id == watch_id).filter(|watch| reachable(&state, watch)).ok_or_else(|| fail("Watch not found in this chat"))?;
                state.services.iter().find(|service| service.resource.id.as_str() == watch.service && service.resource.generation.get() == watch.generation).map(|service| service.binding_snapshot())
            };
            if let Some(service) = &service
                && service.resource.phase.container().is_some()
                && runner(service, &["watch-rm", &watch_id], "")? != Some(0)
            {
                return Err(fail("The service could not stop the watch"));
            }
            let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
            let index = state.watches.iter().position(|watch| watch.id == watch_id).ok_or_else(|| fail("Watch not found in this chat"))?;
            let previous = state.watches[index].stopped;
            state.watches[index].stopped = true;
            if let Err(error) = crate::storage::save(&state) {
                state.watches[index].stopped = previous;
                return Err(error);
            }
            Ok(state.watches[index].summary())
        }
    }
}
