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
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_parallel_runs: 2,
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
#[inline]
pub fn next(runtime: &Runtime) -> Option<usize> {
    let capacity=Capacity::new(runtime.execution.max_parallel_runs.min(MAX_PARALLEL))?;
    scheduling::next(runtime.runs.iter(),capacity,runtime.paused.is_some())
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
        assert_eq!(load_limits_in(&file, None).unwrap().max_parallel_runs, 2);
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
        assert_eq!(restored.execution.max_parallel_runs, 2);
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
}
