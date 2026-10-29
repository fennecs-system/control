//! Deterministic admission and serial execution. Child agents are queued work,
//! not permission to fan out provider calls or share writable workspaces.
use crate::runtime::Runtime;

pub const MAX_PENDING: usize = 32;

pub fn admit(runtime: &Runtime) -> Result<(), &'static str> {
    if runtime.runs.iter().filter(|r| matches!(r.status.as_str(), "queued" | "running" | "cancelling" | "waiting")).count() >= MAX_PENDING {
        Err("Run queue is full (32 pending tasks). Wait for work to finish or cancel a task.")
    } else {
        Ok(())
    }
}

pub fn next(runtime: &Runtime) -> Option<usize> {
    if runtime.runs.iter().any(|r| matches!(r.status.as_str(), "running" | "cancelling")) {
        return None;
    }
    runtime.runs.iter().enumerate().filter(|(_, r)| r.status == "queued")
        .filter(|(_, r)| !runtime.runs.iter().any(|paused| paused.status == "waiting" && paused.chat_id.is_some() && paused.chat_id == r.chat_id && paused.question.as_ref().is_some_and(|q| q.answer.is_none())))
        .min_by_key(|(index, r)| (r.queue_order, *index)).map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Action, Actor};

    #[test]
    fn admission_is_bounded_and_dispatch_is_serial_fifo() {
        let mut runtime = Runtime::default();
        for n in 0..MAX_PENDING {
            runtime.enqueue_for(format!("Task {n}"), "chat", "test", Actor::LocalOperator, Action::Task).unwrap();
        }
        assert!(runtime.enqueue_for("Overflow".into(), "chat", "test", Actor::LocalOperator, Action::Task).is_err());
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
