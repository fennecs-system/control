/// Milliseconds since the Unix epoch, supplied by the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Now(pub u64);

/// The state an effect was planned against. Interpreters compare it with the
/// current state immediately before execution; a durable checkpoint alone does
/// not make an effect eligible.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Stamp<R> {
    pub resource: R,
    pub generation: u64,
    pub revision: u64,
}

/// `Intent` effects run only after the state that planned them is durable, and
/// must be recorded so recovery can reconcile an uncertain outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Durability {
    Intent,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned<R, E> {
    pub stamp: Stamp<R>,
    pub durability: Durability,
    pub effect: E,
}

pub trait Machine {
    type Resource;
    type Event;
    type Effect;
    fn step(&mut self, event: Self::Event, now: Now) -> Vec<Planned<Self::Resource, Self::Effect>>;
}

#[cfg(test)]
mod tests;
