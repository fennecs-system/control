//! Bounded node execution with deterministic per-conversation ownership.
use crate::runtime::Runtime;
use std::io;
use control_core::scheduling::{self, AttemptStamp, Capacity, Phase};

#[cfg(test)]
pub use control_core::scheduling::MAX_PENDING;
pub use control_core::scheduling::MAX_PARALLEL;
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub max_parallel_runs: usize,
    pub max_agents_per_root: usize,
    pub max_pending_agent_intents: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_parallel_runs: 3,
            max_agents_per_root: 64,
            max_pending_agent_intents: 256,
        }
    }
}
impl Limits {
    fn validate(self) -> io::Result<Self> {
        if !(1..=MAX_PARALLEL).contains(&self.max_parallel_runs) {
            return Err(io::Error::other(
                "max_parallel_runs must be between 1 and 8",
            ));
        }
        if !(1..=65536).contains(&self.max_agents_per_root)
            || !(1..=65536).contains(&self.max_pending_agent_intents) {
            return Err(io::Error::other("Agent root and pending-intent budgets must be positive integers up to 65536"));
        }
        Ok(self)
    }
}
pub fn load_limits() -> io::Result<Limits> {
    load_limits_in(
        &crate::config::directory()
            .map_err(io::Error::other)?
            .join("execution.json"),
        std::env::var_os("CONTROL_MAX_PARALLEL_RUNS"),
    )
}
fn load_limits_in(
    path: &std::path::Path,
    override_limit: Option<std::ffi::OsString>,
) -> io::Result<Limits> {
    let mut limits = match crate::config::read_regular(path, 4096, true) {
        Ok(bytes) => serde_json::from_slice::<Limits>(&bytes)
            .map_err(|_| io::Error::other("Invalid execution.json"))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Limits::default(),
        Err(error) => return Err(error),
    };
    if let Some(value) = override_limit {
        limits.max_parallel_runs =
            value.to_str().and_then(|s| s.parse().ok()).ok_or_else(|| {
                io::Error::other("CONTROL_MAX_PARALLEL_RUNS must be an integer from 1 to 8")
            })?;
    }
    limits.validate()
}

#[inline]
pub(crate) fn phase(run: &crate::runtime::Run) -> Phase {
    match run.status.as_str() {
        "queued" => Phase::Queued,
        "running" => Phase::Running,
        "cancelling" => Phase::Cancelling,
        "waiting" => Phase::Waiting {reserves_chat:run.delegation.is_some() || run.question.as_ref().is_some_and(|q|q.answer.is_none())},
        _ => Phase::Inactive,
    }
}
#[inline]
pub(crate) fn stamp(attempt: &crate::usage::Attempt) -> AttemptStamp<'_> {
    AttemptStamp {workspace:attempt.workspace_id.as_deref(),started_unix:attempt.started_unix}
}
#[inline]
pub fn admit(runtime: &Runtime) -> Result<(), &'static str> {
    if scheduling::has_capacity(runtime.runs.iter()) { Ok(()) }
    else { Err("Run queue is full (32 pending tasks). Wait for work to finish or cancel a task.") }
}
/// Delegated and spawned agents and automatic bot turns run in the background
/// lane, whatever their ancestry.
fn background(run: &crate::runtime::Run) -> bool {
    run.source == "bot" || run.persona.as_ref().is_some_and(|persona| persona.parent.is_some())
}
/// Only each chat's oldest queued turn is a candidate, keeping chat FIFO. Among
/// candidates, conversation turns start before background agents, and background
/// agents use at most all but one slot, so a person is not queued behind them.
pub fn next(runtime: &Runtime) -> Option<usize> {
    let capacity=Capacity::new(runtime.execution.max_parallel_runs.min(MAX_PARALLEL))?;
    if runtime.paused.is_some() {
        return None;
    }
    let mut heads=std::collections::HashMap::new();
    for (index, run) in runtime.runs.iter().enumerate().filter(|(_, run)| run.status == "queued") {
        if let Some(chat)=run.chat_id.as_deref() {
            heads.entry(chat).and_modify(|head: &mut (u64, usize)| *head=(*head).min((run.queue_order, index))).or_insert((run.queue_order, index));
        }
    }
    let candidate=|index: usize, run: &crate::runtime::Run| run.status == "queued"
        && run.chat_id.as_deref().is_none_or(|chat| heads.get(chat).is_some_and(|head| head.1 == index));
    let mut lanes=runtime.runs.iter().enumerate()
        .map(|(index, run)| Lane {run, queued: candidate(index, run) && !background(run)})
        .collect::<Vec<_>>();
    if let Some(index)=scheduling::next(lanes.iter(),capacity,false) {
        return Some(index);
    }
    let busy=runtime.runs.iter().filter(|run| background(run) && scheduling::RunView::occupies_slot(*run)).count();
    if busy + usize::from(capacity.slots() > 1) >= capacity.slots() {
        return None;
    }
    for (index, lane) in lanes.iter_mut().enumerate() {
        lane.queued=candidate(index, lane.run) && background(lane.run);
    }
    scheduling::next(lanes.iter(),capacity,false)
}
/// One lane's view of the ledger: other lanes' queued runs are hidden, while
/// every run's slot and chat reservations still count.
struct Lane<'a> {
    run: &'a crate::runtime::Run,
    queued: bool,
}
impl scheduling::RunView for Lane<'_> {
    fn phase(&self) -> Phase {
        if self.run.status == "queued" && !self.queued { Phase::Inactive } else { phase(self.run) }
    }
    fn chat(&self) -> Option<&str> { self.run.chat_id.as_deref() }
    fn queue_order(&self) -> u64 { self.run.queue_order }
    fn occupies_slot(&self) -> bool { scheduling::RunView::occupies_slot(self.run) }
    fn is_queued(&self) -> bool { self.queued }
    fn is_pending(&self) -> bool { scheduling::RunView::is_pending(self.run) }
    fn reserves_chat(&self) -> bool { scheduling::RunView::reserves_chat(self.run) }
}
impl scheduling::RunView for crate::runtime::Run {
    #[inline]
    fn phase(&self) -> Phase { phase(self) }
    #[inline]
    fn chat(&self) -> Option<&str> { self.chat_id.as_deref() }
    #[inline]
    fn queue_order(&self) -> u64 { self.queue_order }
    #[inline]
    fn occupies_slot(&self) -> bool { matches!(self.status.as_str(), "running"|"cancelling") }
    #[inline]
    fn is_queued(&self) -> bool { self.status == "queued" }
    #[inline]
    fn is_pending(&self) -> bool { matches!(self.status.as_str(),"queued"|"running"|"cancelling"|"waiting") }
    #[inline]
    fn reserves_chat(&self) -> bool { self.occupies_slot() || (self.status=="waiting" && (self.delegation.is_some() || self.question.as_ref().is_some_and(|q|q.answer.is_none()))) }
}

#[cfg(test)]
#[path = "scheduler_qa.rs"]
mod qa;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Action, Actor};

    #[test]
    fn slots_overlap_chats_but_never_reorder_or_overlap_a_conversation() {
        let mut runtime = Runtime::default();
        runtime.execution.max_parallel_runs = 2;
        for chat in ["a", "a", "b", "c"] {
            runtime.enqueue("Task".into(), "chat", "test");
            runtime.runs.last_mut().unwrap().chat_id = Some(chat.into());
        }
        assert_eq!(next(&runtime), Some(0));
        runtime.runs[0].status = "running".into();
        assert_eq!(next(&runtime), Some(2));
        runtime.runs[2].status = "cancelling".into();
        assert!(next(&runtime).is_none());
        runtime.runs[0].status = "completed".into();
        assert_eq!(next(&runtime), Some(1));
        runtime.runs[0].status = "waiting".into();
        runtime.runs[0].question = Some(crate::design::Question {
            id: "q".into(),
            nonce: "n".into(),
            generation: "g".into(),
            input: crate::design::Input {
                question: "Pick".into(),
                options: vec!["a".into(), "b".into()],
                recommended: None,
            },
            answer: None,
            continuation: None,
        });
        assert_eq!(
            next(&runtime),
            Some(3),
            "Paused questions reserve their chat, not a model slot"
        );
        runtime.runs[0].question = None;
        runtime.runs[0].delegation = Some(crate::delegation::Delegation {
            input: crate::delegation::Input {
                objective: "Inspect".into(),
                role: crate::delegation::Role::Worker,
            },
            nonce: "n".into(),
            generation: "g".into(),
            backend: crate::backend::Backend::Codex,
            model: "test".into(),
            child: Some("b".into()),
            continuation: None,
        });
        assert_eq!(
            next(&runtime),
            Some(3),
            "Delegation reserves its chat but releases its slot"
        );
        runtime.paused = Some("Uncertain resource ownership".into());
        assert!(next(&runtime).is_none());
    }
    #[test]
    fn private_limits_are_validated_and_do_not_come_from_saved_runtime_state() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!(
            "control-limits-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join("execution.json");
        assert_eq!(load_limits_in(&file, None).unwrap().max_parallel_runs, 3);
        assert_eq!(load_limits_in(&file, None).unwrap().max_agents_per_root,64);
        assert_eq!(load_limits_in(&file, None).unwrap().max_pending_agent_intents,256);
        std::fs::write(&file, b"{\"max_parallel_runs\":4}").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(load_limits_in(&file, None).unwrap().max_parallel_runs, 4);
        assert_eq!(
            load_limits_in(&file, Some("1".into()))
                .unwrap()
                .max_parallel_runs,
            1
        );
        for invalid in ["0", "9", "wrong"] {
            assert!(load_limits_in(&file, Some(invalid.into())).is_err());
        }
        for invalid in [b"{\"max_agents_per_root\":0}".as_slice(),b"{\"max_pending_agent_intents\":0}".as_slice(),b"{\"max_agents_per_root\":65537}".as_slice(),b"{\"max_pending_agent_intents\":\"infinity\"}".as_slice()] {
            std::fs::write(&file,invalid).unwrap();assert!(load_limits_in(&file,None).is_err());
        }
        std::fs::write(&file,b"{\"max_parallel_runs\":2,\"max_agents_per_root\":100,\"max_pending_agent_intents\":500}").unwrap();
        assert_eq!(load_limits_in(&file,None).unwrap().max_agents_per_root,100);
        std::fs::write(&file, b"{\"unlimited\":true}").unwrap();
        assert!(load_limits_in(&file, None).is_err());
        std::fs::write(&file, b"{}").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_limits_in(&file, None).is_err());
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink("missing", &file).unwrap();
        assert!(load_limits_in(&file, None).is_err());
        let restored: Runtime =
            serde_json::from_value(serde_json::json!({"execution":{"max_parallel_runs":1000}}))
                .unwrap();
        assert_eq!(restored.execution.max_parallel_runs, 3);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn admission_is_bounded_and_dispatch_is_serial_fifo() {
        let mut runtime = Runtime::default();
        runtime.execution.max_parallel_runs = 1;
        for n in 0..MAX_PENDING {
            runtime
                .enqueue_for(
                    format!("Task {n}"),
                    "chat",
                    "test",
                    Actor::LocalOperator,
                    Action::Task,
                )
                .unwrap();
        }
        assert!(
            runtime
                .enqueue_for(
                    "Overflow".into(),
                    "chat",
                    "test",
                    Actor::LocalOperator,
                    Action::Task
                )
                .is_err()
        );
        assert_eq!(runtime.runs.len(), MAX_PENDING);
        assert_eq!(next(&runtime), Some(0));
        runtime.runs[0].status = "running".into();
        assert_eq!(next(&runtime), None);
        runtime.runs[0].status = "cancelling".into();
        assert_eq!(next(&runtime), None);
        runtime.runs[0].status = "completed".into();
        assert_eq!(next(&runtime), Some(1));
        assert!(admit(&runtime).is_ok());
    }
    #[test]
    fn background_agents_leave_a_slot_for_conversations_and_queue_behind_them() {
        let persona = |parent: Option<&str>| Some(crate::delegation::Persona {
            role: crate::delegation::Role::Worker,
            root: "main".into(),
            depth: u8::from(parent.is_some()),
            parent: parent.map(Into::into),
        });
        let enqueue = |runtime: &mut Runtime, chat: &str, agent: bool| {
            runtime.enqueue("Task".into(), "chat", "test");
            let run = runtime.runs.last_mut().unwrap();
            run.chat_id = Some(chat.into());
            if agent { run.persona = persona(Some("main")); }
        };
        let mut runtime = Runtime::default();
        runtime.execution.max_parallel_runs = 3;
        for chat in ["alice", "cass", "scout"] { enqueue(&mut runtime, chat, true); }
        enqueue(&mut runtime, "main", false);
        assert_eq!(next(&runtime), Some(3), "a conversation turn starts before older queued agents");
        runtime.runs[3].status = "running".into();
        assert_eq!(next(&runtime), Some(0));
        runtime.runs[0].status = "running".into();
        assert_eq!(next(&runtime), Some(1));
        runtime.runs[1].status = "running".into();
        assert_eq!(next(&runtime), None);
        runtime.runs[3].status = "completed".into();
        assert_eq!(next(&runtime), None, "agents use at most all but one slot");
        enqueue(&mut runtime, "main", false);
        runtime.runs[4].persona = persona(None);
        assert_eq!(next(&runtime), Some(4), "root-chat continuations use the conversation lane");
        runtime.runs[4].status = "running".into();
        runtime.runs[0].status = "completed".into();
        assert_eq!(next(&runtime), Some(2), "agents may fill all but one slot while a conversation runs");
        runtime.runs[2].status = "running".into();
        runtime.runs[4].status = "completed".into();
        assert_eq!(next(&runtime), None);
        runtime.paused = Some(Default::default());
        assert_eq!(next(&runtime), None);

        let mut serial = Runtime::default();
        serial.execution.max_parallel_runs = 1;
        enqueue(&mut serial, "alice", true);
        enqueue(&mut serial, "main", false);
        assert_eq!(next(&serial), Some(1), "a single slot still goes to the conversation first");
        serial.runs[1].status = "completed".into();
        assert_eq!(next(&serial), Some(0), "a single slot can still run background agents");
    }
    #[test]
    fn lane_priority_never_reorders_turns_within_a_chat() {
        let enqueue = |runtime: &mut Runtime, chat: &str, agent: bool| {
            runtime.enqueue("Task".into(), "chat", "test");
            let run = runtime.runs.last_mut().unwrap();
            run.chat_id = Some(chat.into());
            if agent {
                run.persona = Some(crate::delegation::Persona { role: crate::delegation::Role::Worker, root: "main".into(), depth: 1, parent: Some("main".into()) });
            }
        };
        let mut runtime = Runtime::default();
        runtime.execution.max_parallel_runs = 3;
        enqueue(&mut runtime, "alice", true);
        enqueue(&mut runtime, "alice", false);
        enqueue(&mut runtime, "main", false);
        assert_eq!(next(&runtime), Some(2), "only each chat's oldest queued turn is a candidate");
        runtime.runs[2].status = "running".into();
        assert_eq!(next(&runtime), Some(0), "a queued agent turn runs before a later message in its chat");
        runtime.runs[0].status = "running".into();
        assert_eq!(next(&runtime), None);
        runtime.runs[0].status = "completed".into();
        assert_eq!(next(&runtime), Some(1));

        let mut tied = Runtime::default();
        tied.execution.max_parallel_runs = 3;
        enqueue(&mut tied, "bob", true);
        enqueue(&mut tied, "bob", false);
        for run in &mut tied.runs { run.queue_order = 7; }
        assert_eq!(next(&tied), Some(0), "equal queue order falls back to ledger order within a chat");
    }
    #[test]
    fn automatic_bot_turns_of_root_chats_leave_a_slot_for_people() {
        let mut runtime = Runtime::default();
        runtime.execution.max_parallel_runs = 2;
        for chat in ["farm", "mine"] {
            runtime.enqueue("Woke".into(), "chat", "bot");
            runtime.runs.last_mut().unwrap().chat_id = Some(chat.into());
        }
        assert_eq!(next(&runtime), Some(0));
        runtime.runs[0].status = "running".into();
        assert_eq!(next(&runtime), None, "a second bot may not take the last slot");
        runtime.enqueue("Hello".into(), "chat", "web");
        runtime.runs.last_mut().unwrap().chat_id = Some("main".into());
        assert_eq!(next(&runtime), Some(2));
    }
}
