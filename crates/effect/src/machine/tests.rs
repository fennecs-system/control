
use super::*;

#[derive(Default)]
struct Lease {
    revision: u64,
    holder: Option<&'static str>,
    expires: u64,
}
#[derive(Debug, PartialEq)]
enum Event {
    Acquire(&'static str, u64),
    Tick,
}
#[derive(Debug, PartialEq)]
enum Effect {
    Start(&'static str),
    Stop(&'static str),
}
impl Machine for Lease {
    type Resource = &'static str;
    type Event = Event;
    type Effect = Effect;
    fn step(&mut self, event: Event, now: Now) -> Vec<Planned<&'static str, Effect>> {
        let planned = |revision, durability, effect| Planned {
            stamp: Stamp {
                resource: "lease",
                generation: 1,
                revision,
            },
            durability,
            effect,
        };
        match event {
            Event::Acquire(holder, ttl) if self.holder.is_none() => {
                self.revision += 1;
                self.holder = Some(holder);
                self.expires = now.0 + ttl;
                vec![planned(
                    self.revision,
                    Durability::Intent,
                    Effect::Start(holder),
                )]
            }
            Event::Tick if self.holder.is_some() && now.0 >= self.expires => {
                self.revision += 1;
                let holder = self.holder.take().unwrap();
                vec![planned(
                    self.revision,
                    Durability::None,
                    Effect::Stop(holder),
                )]
            }
            _ => Vec::new(),
        }
    }
}

#[test]
fn steps_are_deterministic_functions_of_state_event_and_time() {
    let mut lease = Lease::default();
    let started = lease.step(Event::Acquire("bot", 100), Now(1_000));
    assert_eq!(started.len(), 1);
    assert_eq!(
        (
            started[0].durability,
            &started[0].effect,
            started[0].stamp.revision
        ),
        (Durability::Intent, &Effect::Start("bot"), 1)
    );
    assert!(
        lease
            .step(Event::Acquire("other", 100), Now(1_010))
            .is_empty()
    );
    assert!(lease.step(Event::Tick, Now(1_099)).is_empty());
    let stopped = lease.step(Event::Tick, Now(1_100));
    assert_eq!(
        (
            stopped[0].durability,
            &stopped[0].effect,
            stopped[0].stamp.revision
        ),
        (Durability::None, &Effect::Stop("bot"), 2)
    );
    assert_ne!(started[0].stamp, stopped[0].stamp);
}
