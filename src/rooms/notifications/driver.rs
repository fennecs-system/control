//! Blocking interpreter. Own the state borrow through checkpoint acknowledgement.
use super::*;
use crate::application::room_notifications as operation;

struct InFlight<'a> {
    state: &'a mut State,
    watch: usize,
    pending: Option<operation::Pending>,
    durable: bool,
}
fn edit(state: &mut State, watch: usize) -> operation::Edit<'_> {
    let watch = &mut state.collective.room_watches[watch];
    operation::Edit {
        runs: &mut state.runtime.runs,
        notice: &mut state.runtime.notice,
        chats: &mut state.chats,
        wake: &mut watch.wake,
        delivered: &mut watch.delivered,
    }
}
impl<'a> InFlight<'a> {
    fn new(state: &'a mut State, watch: usize, change: operation::Change) -> Self {
        let pending = change.apply(edit(state, watch));
        Self {
            state,
            watch,
            pending: Some(pending),
            durable: false,
        }
    }
    fn next(&self) -> Option<operation::Effect> {
        self.pending.as_ref().unwrap().next()
    }
    fn completed(&mut self, effect: operation::Effect) {
        self.pending.as_mut().unwrap().completed(effect);
        if effect == operation::Effect::Checkpoint {
            self.durable = true;
        }
    }
    fn finish(mut self) {
        assert!(self.next().is_none());
        self.pending.take();
    }
}
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if !self.durable
            && let Some(pending) = self.pending.take()
        {
            pending.rollback(edit(self.state, self.watch));
        }
    }
}
pub(super) fn publish(
    state: &mut State,
    watch: usize,
    change: operation::Change,
    mut save: impl FnMut(&State) -> std::io::Result<()>,
    wake: impl FnOnce(),
) -> Result<(), &'static str> {
    let queued = matches!(change, operation::Change::Queue(_));
    let mut flight = InFlight::new(state, watch, change);
    let mut wake = Some(wake);
    while let Some(effect) = flight.next() {
        match effect {
            operation::Effect::Checkpoint => save(flight.state).map_err(|_| {
                if queued {
                    "Could not save room notification; wake remains pending."
                } else {
                    "Could not save room subscription progress."
                }
            })?,
            operation::Effect::WakeDispatcher => wake.take().unwrap()(),
        }
        flight.completed(effect);
    }
    flight.finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use operation::Effect;
    use std::{
        cell::RefCell,
        future::Future,
        panic::{AssertUnwindSafe, catch_unwind},
    };

    fn fixture() -> State {
        let (mut state, id) = super::super::tests::fixture();
        call(
            &mut state,
            &id,
            "control_room_watch",
            &serde_json::json!({"enabled":true,"wake":true}),
        )
        .unwrap();
        state.runtime.runs[0].status = "completed".into();
        state
    }
    fn queued(state: &State) -> operation::Change {
        let watch = &state.collective.room_watches[0];
        operation::queue(
            &state.runtime.runs,
            &state.runtime.runs[0],
            0,
            &watch.chat,
            &watch.room,
            "fixed-notification".into(),
            Vec::new(),
        )
        .unwrap()
    }
    fn snapshot(state: &State) -> Vec<u8> {
        serde_json::to_vec(state).unwrap()
    }

    #[test]
    fn checkpoint_failure_or_unwind_restores_publication_without_dispatch() {
        for panic in [false, true] {
            let mut state = fixture();
            let before = snapshot(&state);
            let change = queued(&state);
            let result = catch_unwind(AssertUnwindSafe(|| {
                publish(
                    &mut state,
                    0,
                    change,
                    |_| {
                        if panic {
                            panic!("injected checkpoint panic");
                        }
                        Err(std::io::Error::other("injected checkpoint failure"))
                    },
                    || panic!("must not dispatch an uncommitted run"),
                )
            }));
            if panic {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert_eq!(snapshot(&state), before);
        }
        for change in [operation::Change::Advance(17), operation::Change::Disarm] {
            let mut state = fixture();
            let before = snapshot(&state);
            assert!(
                publish(
                    &mut state,
                    0,
                    change,
                    |_| Err(std::io::Error::other("disk")),
                    || panic!("cursor changes never dispatch")
                )
                .is_err()
            );
            assert_eq!(snapshot(&state), before);
        }
    }

    #[test]
    fn blocking_and_tokio_interpreters_share_results_and_ordered_effects() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for fail in [false, true] {
            let mut blocking = fixture();
            let mut asynchronous: State = serde_json::from_slice(&snapshot(&blocking)).unwrap();
            let trace = RefCell::new(Vec::new());
            let change = queued(&blocking);
            let first = publish(
                &mut blocking,
                0,
                change,
                |state| {
                    trace.borrow_mut().push(Effect::Checkpoint);
                    let _candidate = snapshot(state);
                    if fail {
                        Err(std::io::Error::other("disk"))
                    } else {
                        Ok(())
                    }
                },
                || trace.borrow_mut().push(Effect::WakeDispatcher),
            );
            let mut other_trace = Vec::new();
            let second: Result<(), &'static str> = runtime.block_on(async {
                let change = queued(&asynchronous);
                let mut flight = InFlight::new(&mut asynchronous, 0, change);
                while let Some(effect) = flight.next() {
                    tokio::task::yield_now().await;
                    other_trace.push(effect);
                    if effect == Effect::Checkpoint {
                        let _candidate = snapshot(flight.state);
                        if fail {
                            return Err("Could not save room notification; wake remains pending.");
                        }
                    }
                    flight.completed(effect);
                }
                flight.finish();
                Ok(())
            });
            assert_eq!(first, second);
            assert_eq!(*trace.borrow(), other_trace);
            assert_eq!(
                *trace.borrow(),
                if fail {
                    vec![Effect::Checkpoint]
                } else {
                    vec![Effect::Checkpoint, Effect::WakeDispatcher]
                }
            );
            assert_eq!(snapshot(&blocking), snapshot(&asynchronous));
            // A restarted driver sees the durable consumed wake, not another job.
            let mut restored: State = serde_json::from_slice(&snapshot(&blocking)).unwrap();
            if !fail {
                assert!(restored.collective.room_watches[0].wake.is_none());
                reconcile(&mut restored, |_| panic!("must not publish twice")).unwrap();
                assert_eq!(snapshot(&restored), snapshot(&blocking));
            }
        }
    }

    #[test]
    fn dropping_a_suspended_transaction_preserves_only_confirmed_checkpoints() {
        for committed in [false, true] {
            let mut state = fixture();
            let before = snapshot(&state);
            let change = queued(&state);
            let mut future = Box::pin(async {
                let mut flight = InFlight::new(&mut state, 0, change);
                if committed {
                    flight.completed(Effect::Checkpoint);
                }
                std::future::pending::<()>().await;
                flight.finish();
            });
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(future.as_mut().poll(&mut context).is_pending());
            drop(future);
            if committed {
                assert_ne!(snapshot(&state), before);
                assert_eq!(state.runtime.runs.last().unwrap().id, "fixed-notification");
                assert!(state.collective.room_watches[0].wake.is_none());
            } else {
                assert_eq!(snapshot(&state), before);
            }
        }
        let mut state = fixture();
        let change = queued(&state);
        assert!(
            catch_unwind(AssertUnwindSafe(|| publish(
                &mut state,
                0,
                change,
                |_| Ok(()),
                || panic!("injected dispatcher panic")
            )))
            .is_err()
        );
        assert_eq!(state.runtime.runs.last().unwrap().id, "fixed-notification");
        assert!(state.collective.room_watches[0].wake.is_none());
    }
}
