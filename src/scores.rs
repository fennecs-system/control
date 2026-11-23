//! Workload score probes. A chat declares a bounded command in one of its
//! services that prints a small JSON object of numbers on its last line, such
//! as `{"blocks_placed":75,"beds":2}`, `{"items":120}` or `{"alerts":3}`.
//! Control runs it on a schedule and when its team reaches a milestone (a turn
//! completes or a claim is done anywhere in the team), and keeps a small,
//! capped series per probe. A team root's probes score the whole team.
//!
//! Control never interprets the names or values: they are the workload's own
//! untrusted claims, kept as evidence for operators and offline scorecards
//! (cost per outcome, progress per model turn). Reads are operator-only
//! (`control.run.scores`) plus one line in the chat view.
use crate::{
    State,
    tool_registry::{AttemptCall, AttemptRef, AttemptTools, LocalHost},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Probes on this node; each sample is one small object, so the whole store
/// stays well under a megabyte at the caps.
pub const MAX_PROBES: usize = 32;
pub const MAX_PER_CHAT: usize = 4;
/// Samples kept per probe. When full, the older half is thinned to every
/// second sample, so a series keeps its whole span at falling resolution.
pub const MAX_SAMPLES: usize = 240;
pub const MAX_KEYS: usize = 8;
pub const MAX_KEY_CHARS: usize = 48;
pub const MAX_COMMAND_BYTES: usize = 2000;
const MAX_LINE_BYTES: usize = 2048;
pub const DEFAULT_INTERVAL: u64 = 300;
pub const MIN_INTERVAL: u64 = 60;
pub const MAX_INTERVAL: u64 = 86_400;
/// Milestone samples come at most this often per probe.
const MILESTONE_GAP: u64 = 30;
/// Scheduled and milestone samples; the declaring call's test run is shorter
/// so its reply stays inside the worker bridge's timeout.
const TIMEOUT: Duration = Duration::from_secs(20);
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    /// The declaring chat. A team root's probe scores its whole team.
    pub chat: String,
    pub name: String,
    pub service: String,
    pub generation: u64,
    pub command: String,
    pub interval_seconds: u64,
    pub declared_unix: u64,
    /// Last sampling attempt, successful or not.
    #[serde(default)]
    pub last_unix: u64,
    /// Team milestones counted at the last sample.
    #[serde(default)]
    pub milestones: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default)]
    pub samples: VecDeque<Sample>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub unix: u64,
    pub trigger: Trigger,
    pub values: BTreeMap<String, f64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Declared,
    Schedule,
    Milestone,
}

use crate::config::unix_seconds as now;

/// The score on the output's last non-empty line: a JSON object of 1 to
/// [`MAX_KEYS`] finite numbers under short plain names. Earlier lines are free
/// diagnostics.
pub fn parse(output: &str) -> Result<BTreeMap<String, f64>, String> {
    let line = output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or("The probe printed nothing")?;
    if line.len() > MAX_LINE_BYTES {
        return Err(format!("The probe's last line exceeds {MAX_LINE_BYTES} bytes"));
    }
    let object = serde_json::from_str::<serde_json::Map<String, Value>>(line)
        .map_err(|_| "The probe's last line must be a JSON object such as {\"items\":120}".to_owned())?;
    if object.is_empty() || object.len() > MAX_KEYS {
        return Err(format!("A score needs 1 to {MAX_KEYS} numeric fields"));
    }
    object
        .into_iter()
        .map(|(key, value)| {
            let named = !key.is_empty()
                && key.chars().count() <= MAX_KEY_CHARS
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
            match value.as_f64().filter(|number| number.is_finite()) {
                Some(number) if named => Ok((key, number)),
                _ if !named => Err(format!(
                    "Score names are 1 to {MAX_KEY_CHARS} letters, digits, '_', '-' or '.'"
                )),
                _ => Err(format!("Score field {key} is not a number")),
            }
        })
        .collect()
}

/// Appends a sample, thinning the older half when the series is full.
pub fn push(samples: &mut VecDeque<Sample>, sample: Sample) {
    if samples.len() >= MAX_SAMPLES {
        let half = samples.len() / 2;
        let mut index = 0;
        samples.retain(|_| {
            let keep = index >= half || index % 2 == 0;
            index += 1;
            keep
        });
    }
    samples.push_back(sample);
}

/// Completed turns plus done claims across the team of `chat`: the count whose
/// increase triggers a milestone sample.
pub fn milestones(state: &State, chat: &str) -> u64 {
    let teams = Teams::new(state);
    teams
        .root(chat)
        .and_then(|root| teams.milestones(state, &[root]).get(root).copied())
        .unwrap_or(0)
}
/// Each chat's team root, computed once per pass: one walk over the chats and
/// one over runs and claims, however many probes there are.
struct Teams<'a> {
    roots: HashMap<&'a str, &'a str>,
}
impl<'a> Teams<'a> {
    fn new(state: &'a State) -> Self {
        let parents = state
            .chats
            .iter()
            .map(|chat| (chat.id.as_str(), chat.parent_chat.as_deref()))
            .collect::<HashMap<_, _>>();
        let roots = parents
            .keys()
            .map(|&id| {
                let root = std::iter::successors(Some(id), |id| {
                    parents
                        .get(id)
                        .copied()
                        .flatten()
                        .filter(|parent| parents.contains_key(parent))
                })
                .take(64)
                .last()
                .unwrap_or(id);
                (id, root)
            })
            .collect();
        Self { roots }
    }
    fn root(&self, chat: &str) -> Option<&'a str> {
        self.roots.get(chat).copied()
    }
    /// Milestones of the teams rooted at `wanted`.
    fn milestones(&self, state: &State, wanted: &[&'a str]) -> HashMap<&'a str, u64> {
        let mut counts = wanted.iter().map(|root| (*root, 0)).collect::<HashMap<_, u64>>();
        let mut count = |chat: &str| {
            if let Some(total) = self.root(chat).and_then(|root| counts.get_mut(root)) {
                *total += 1
            }
        };
        for run in state
            .runtime
            .runs
            .iter()
            .filter(|run| run.status == "completed" && !run.attempts.is_empty())
        {
            if let Some(chat) = run.chat_id.as_deref() {
                count(chat)
            }
        }
        for claim in state
            .collective
            .claims
            .iter()
            .filter(|claim| claim.status == crate::rooms::claims::Status::Done)
        {
            count(&claim.owner);
        }
        counts
    }
}

/// A probe due now and what to run it with, read under the lock.
struct Due {
    chat: String,
    name: String,
    trigger: Trigger,
    milestones: u64,
    target: Result<(crate::services::Service, String), String>,
}
fn due(state: &State, time: u64) -> Vec<Due> {
    if state.scores.is_empty() {
        return Vec::new();
    }
    let teams = Teams::new(state);
    let roots = state
        .scores
        .iter()
        .filter_map(|probe| teams.root(&probe.chat))
        .collect::<Vec<_>>();
    let counts = teams.milestones(state, &roots);
    state
        .scores
        .iter()
        .filter_map(|probe| {
            let milestones = teams
                .root(&probe.chat)
                .and_then(|root| counts.get(root))
                .copied()
                .unwrap_or(0);
            let trigger = if time >= probe.last_unix.saturating_add(probe.interval_seconds) {
                Trigger::Schedule
            } else if milestones > probe.milestones && time >= probe.last_unix.saturating_add(MILESTONE_GAP) {
                Trigger::Milestone
            } else {
                return None;
            };
            let target = crate::services::probe_target(state, &probe.service, |service| {
                service.resource.generation.get() == probe.generation
            })
            .map(|service| (service, probe.command.clone()))
            .ok_or_else(|| {
                "The probe's service has stopped or was replaced; declare the probe again with a running service."
                    .to_owned()
            });
            Some(Due {
                chat: probe.chat.clone(),
                name: probe.name.clone(),
                trigger,
                milestones,
                target,
            })
        })
        .collect()
}
/// Runs the probe command once, under a reap marker unique to this sample.
fn sample(
    service: &crate::services::Service,
    command: &str,
    timeout: Duration,
) -> Result<BTreeMap<String, f64>, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let owner = format!("score\u{0}{}\u{0}{nonce}", service.resource.id.as_str());
    let output = crate::services::probe(service, command, &owner, timeout).map_err(|error| error.to_string())?;
    if output.timed_out || output.exit_code.is_none() {
        return Err("The probe did not finish within its time bound".into());
    }
    if output.exit_code != Some(0) {
        return Err(format!("The probe exited with status {}", output.status()));
    }
    parse(&output.text)
}
/// Records one sampling attempt. Returns whether the probe still exists.
fn apply(
    state: &mut State,
    chat: &str,
    name: &str,
    time: u64,
    trigger: Trigger,
    milestones: u64,
    outcome: Result<BTreeMap<String, f64>, String>,
) -> bool {
    let Some(probe) = state
        .scores
        .iter_mut()
        .find(|probe| probe.chat == chat && probe.name == name)
    else {
        return false;
    };
    probe.last_unix = time;
    probe.milestones = milestones;
    match outcome {
        Ok(values) => {
            probe.error.clear();
            push(
                &mut probe.samples,
                Sample {
                    unix: time,
                    trigger,
                    values,
                },
            );
        }
        Err(error) => probe.error = error.chars().filter(|c| !c.is_control()).take(300).collect(),
    }
    true
}
/// Probes of chats that no longer exist are dropped.
fn retire(state: &mut State) -> bool {
    let before = state.scores.len();
    let State { scores, chats, .. } = state;
    scores.retain(|probe| chats.iter().any(|chat| chat.id == probe.chat));
    before != state.scores.len()
}

/// Samples due probes until shutdown. Probe commands run without the state lock.
pub fn supervise(shared: Arc<Mutex<State>>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !crate::shutdown::pause(TICK) {
            let pending = match shared.lock() {
                Ok(mut state) => {
                    if retire(&mut state) {
                        let _ = crate::storage::save(&state);
                    }
                    due(&state, now())
                }
                Err(_) => return,
            };
            for item in pending {
                if crate::shutdown::requested() {
                    return;
                }
                let outcome = item
                    .target
                    .and_then(|(service, command)| sample(&service, &command, TIMEOUT));
                let Ok(mut state) = shared.lock() else { return };
                if apply(
                    &mut state,
                    &item.chat,
                    &item.name,
                    now(),
                    item.trigger,
                    item.milestones,
                    outcome,
                ) {
                    let _ = crate::storage::save(&state);
                }
            }
        }
    })
}

fn summary(probe: &Probe) -> Value {
    json!({"chat":probe.chat,"name":probe.name,"service":probe.service,"command":probe.command.chars().take(200).collect::<String>(),
        "interval_seconds":probe.interval_seconds,"declared_unix":probe.declared_unix,"last_unix":probe.last_unix,
        "error":(!probe.error.is_empty()).then_some(&probe.error),"samples":probe.samples})
}
/// `control.run.scores`: every probe in the team (root chat and descendants)
/// of `chat`, with its series. Read-only.
pub fn read(state: &State, chat: &str) -> Result<Value, &'static str> {
    let session = state
        .chats
        .iter()
        .find(|session| session.id == chat)
        .ok_or("Chat not found.")?;
    crate::identity::authorize(&session.actor, crate::identity::Action::Task)?;
    let team = crate::chat::tree(state, chat);
    let root = team
        .iter()
        .copied()
        .find(|id| {
            state.chats.iter().any(|session| {
                session.id == *id
                    && session
                        .parent_chat
                        .as_deref()
                        .is_none_or(|parent| !team.contains(parent))
            })
        })
        .unwrap_or(chat);
    let probes = state
        .scores
        .iter()
        .filter(|probe| team.contains(probe.chat.as_str()))
        .map(summary)
        .collect::<Vec<_>>();
    Ok(json!({"root":root,"milestones":milestones(state, chat),"probes":probes,
        "note":"Values are the workload's own untrusted claims; Control does not interpret them."}))
}
/// One line for the chat view: the latest values of this chat's probes.
pub fn line(state: &State, chat: &str) -> String {
    let parts = state
        .scores
        .iter()
        .filter(|probe| probe.chat == chat)
        .map(|probe| {
            let latest = probe
                .samples
                .back()
                .map(|sample| {
                    sample
                        .values
                        .iter()
                        .map(|(key, value)| format!("{key} {value}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_else(|| "no sample yet".into());
            let error = if probe.error.is_empty() {
                String::new()
            } else {
                format!(" · last probe failed: {}", probe.error)
            };
            format!("{}: {latest} ({} samples){error}", probe.name, probe.samples.len())
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return String::new();
    }
    format!(
        "<p class='muted tiny chat-scores'>Scores · {}</p>",
        crate::escape(&parts.join(" · "))
    )
}

pub enum ScoreCall {
    Declare {
        name: String,
        service_id: String,
        command: String,
        interval_seconds: u64,
    },
    Clear {
        name: String,
    },
}
pub struct Tools;
impl AttemptTools for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![json!({
            "name": "control_score_probe",
            "description": concat!(
                "Declare how this chat's workload is scored, so progress is measured by the workload's own ",
                "numbers rather than by claims or messages. The command runs in one of your services (as the ",
                "service user, in /workspace, 10 s for this test run and 20 s later) and must print a JSON object ",
                "of up to 8 numbers on its last line, e.g. {\"blocks_placed\":75,\"beds\":2}, {\"items\":120} or ",
                "{\"alerts\":3}. It must be quick and read-only: report state your workload already keeps, never ",
                "act. Control runs it now to check it, then every interval_seconds (default 300) and when your ",
                "team completes a turn or a claim, and keeps a capped series the operator can read. A probe on ",
                "the team's root chat scores the whole team. Redeclaring the same name replaces it; clear=true ",
                "removes it. Up to 4 per chat."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string", "minLength": 1, "maxLength": 32, "default": "score"},
                    "service_id": {"type": "string"},
                    "command": {"type": "string", "minLength": 1, "maxLength": MAX_COMMAND_BYTES},
                    "interval_seconds": {
                        "type": "integer",
                        "minimum": MIN_INTERVAL,
                        "maximum": MAX_INTERVAL,
                        "default": DEFAULT_INTERVAL
                    },
                    "clear": {"type": "boolean", "default": false}
                },
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false}
        })]
    }
    fn known(&self, name: &str) -> bool {
        name == "control_score_probe"
    }
    fn call(&self, cx: &AttemptCall<'_>, _name: &str, args: &Value) -> io::Result<Value> {
        execute(cx.host, &cx.attempt, decode(args)?)
    }
}
pub fn decode(args: &Value) -> io::Result<ScoreCall> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        name: Option<String>,
        service_id: Option<String>,
        command: Option<String>,
        interval_seconds: Option<u64>,
        #[serde(default)]
        clear: bool,
    }
    let input = serde_json::from_value::<Input>(args.clone())
        .map_err(|_| io::Error::other("Expected name, service_id, command, interval_seconds or clear"))?;
    let name = input.name.unwrap_or_else(|| "score".into());
    if name.is_empty()
        || name.chars().count() > 32
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(io::Error::other("A probe name is 1 to 32 letters, digits, '_' or '-'"));
    }
    if input.clear {
        return Ok(ScoreCall::Clear { name });
    }
    let (Some(service_id), Some(command)) = (input.service_id, input.command) else {
        return Err(io::Error::other("Declaring a probe needs service_id and command"));
    };
    if command.trim().is_empty() || command.len() > MAX_COMMAND_BYTES {
        return Err(io::Error::other(format!(
            "The command must be 1 to {MAX_COMMAND_BYTES} bytes"
        )));
    }
    let interval_seconds = input.interval_seconds.unwrap_or(DEFAULT_INTERVAL);
    if !(MIN_INTERVAL..=MAX_INTERVAL).contains(&interval_seconds) {
        return Err(io::Error::other(format!(
            "interval_seconds must be {MIN_INTERVAL} to {MAX_INTERVAL}"
        )));
    }
    Ok(ScoreCall::Declare {
        name,
        service_id,
        command,
        interval_seconds,
    })
}
fn execute(host: &LocalHost<'_>, attempt: &AttemptRef<'_>, call: ScoreCall) -> io::Result<Value> {
    let shared = host.0;
    let node = control_core::resources::NodeId::try_from(crate::config::node_id().map_err(io::Error::other)?)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let (caller, chat) = {
        let state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
        let run = state
            .runtime
            .runs
            .iter()
            .find(|run| {
                run.id == attempt.run
                    && run.status == "running"
                    && run.attempts.last().and_then(|a| a.workspace_id.as_deref()) == Some(attempt.generation)
            })
            .ok_or_else(|| io::Error::other("Attempt is not active"))?;
        crate::identity::authorize(&run.actor, run.action).map_err(io::Error::other)?;
        if crate::delegation::is_judge(run) {
            return Err(io::Error::other("Judges cannot declare score probes"));
        }
        let chat = run
            .chat_id
            .clone()
            .ok_or_else(|| io::Error::other("A chat is required"))?;
        (run.clone(), chat)
    };
    match call {
        ScoreCall::Clear { name } => {
            let mut state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
            let previous = state.scores.clone();
            state.scores.retain(|probe| !(probe.chat == chat && probe.name == name));
            let removed = previous.len() != state.scores.len();
            if removed && let Err(error) = crate::storage::save(&state) {
                state.scores = previous;
                return Err(error);
            }
            Ok(json!({"cleared":removed,"name":name}))
        }
        ScoreCall::Declare {
            name,
            service_id,
            command,
            interval_seconds,
        } => {
            let service = {
                let state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
                let replacing = state
                    .scores
                    .iter()
                    .any(|probe| probe.chat == chat && probe.name == name);
                if !replacing && state.scores.iter().filter(|probe| probe.chat == chat).count() >= MAX_PER_CHAT {
                    return Err(io::Error::other(format!(
                        "This chat already has {MAX_PER_CHAT} probes; clear one first"
                    )));
                }
                if !replacing && state.scores.len() >= MAX_PROBES {
                    return Err(io::Error::other(
                        "This node already keeps the maximum number of score probes",
                    ));
                }
                state
                    .services
                    .iter()
                    .find(|service| {
                        service.resource.id.as_str() == service_id
                            && crate::services::accessible(&state, service, &node, &caller)
                    })
                    .ok_or_else(|| io::Error::other("Service is not owned by this chat or its team"))?
                    .binding_snapshot()
            };
            // Test before accepting: a probe that cannot score is rejected now,
            // not discovered failing later.
            let values = sample(&service, &command, TEST_TIMEOUT)
                .map_err(|error| io::Error::other(format!("The probe was not saved: {error}")))?;
            let time = now();
            let mut state = shared.lock().map_err(|_| io::Error::other("State unavailable"))?;
            let milestones = milestones(&state, &chat);
            let mut probe = Probe {
                chat: chat.clone(),
                name: name.clone(),
                service: service_id,
                generation: service.resource.generation.get(),
                command,
                interval_seconds,
                declared_unix: time,
                last_unix: time,
                milestones,
                error: String::new(),
                samples: VecDeque::new(),
            };
            push(
                &mut probe.samples,
                Sample {
                    unix: time,
                    trigger: Trigger::Declared,
                    values: values.clone(),
                },
            );
            let previous = state.scores.clone();
            state
                .scores
                .retain(|existing| !(existing.chat == chat && existing.name == name));
            if state.scores.len() >= MAX_PROBES {
                state.scores = previous;
                return Err(io::Error::other(
                    "This node already keeps the maximum number of score probes",
                ));
            }
            state.scores.push(probe);
            if let Err(error) = crate::storage::save(&state) {
                state.scores = previous;
                return Err(error);
            }
            Ok(json!({"name":name,"values":values,"interval_seconds":interval_seconds,
                "note":"Saved. Control samples it on schedule and at team milestones; the operator reads the series. Keep it quick and read-only."}))
        }
    }
}

#[cfg(test)]
mod tests;
