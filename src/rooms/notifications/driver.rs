//! Blocking interpreter. Own the state borrow through checkpoint acknowledgement.
//! Usage and failure fixtures live in driver/tests.rs.
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
mod tests;
