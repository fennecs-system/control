use control_bot::{BotId, Budget, ChatId, Definition, Effect, Event, Reason, ServiceId, Source, Supervisor, TurnId};
use control_effect::machine::{Machine, Now, Planned};

fn definition() -> Definition {
    Definition { bot: BotId("bot".into()), chat: ChatId("chat-original".into()), sources:vec![Source::Room{room:"room".into()}], budget:Budget{cooldown_ms:10, window_ms:100, turns_per_window:1} }
}
fn message(cursor:u64) -> Event { Event::RoomMessage{room:"room".into(), from:None, cursor} }
fn turn(effects:&[Planned<BotId,Effect>]) -> TurnId {
    effects.iter().find_map(|planned|match planned.effect {Effect::RequestTurn{turn,..}=>Some(turn),_=>None}).expect("expected turn")
}
fn has_turn(effects:&[Planned<BotId,Effect>]) -> bool { effects.iter().any(|planned|matches!(planned.effect,Effect::RequestTurn{..})) }

#[test]
fn paused_timer_should_not_wake_an_idle_driver() {
    let mut supervisor=Supervisor::default();
    let mut def=definition();def.sources=vec![Source::Timer{every_ms:10}];
    supervisor.step(Event::Define(def),Now(0));
    supervisor.step(Event::Pause(BotId("bot".into())),Now(1));
    assert_eq!(supervisor.next_deadline(Now(1)),None,"paused bot cannot emit a turn on Tick");
}

#[test]
fn temporary_window_edit_must_not_erase_turn_accounting() {
    let mut supervisor=Supervisor::default();let original=definition();
    supervisor.step(Event::Define(original.clone()),Now(0));
    let first=turn(&supervisor.step(message(1),Now(0)));
    supervisor.step(Event::TurnCompleted{bot:original.bot.clone(),turn:first},Now(1));
    supervisor.step(message(2),Now(2));
    let mut shorter=original.clone();shorter.budget.window_ms=1;
    assert!(!has_turn(&supervisor.step(Event::Define(shorter),Now(3))));
    assert!(!has_turn(&supervisor.step(Event::Define(original),Now(4))));
    assert!(!has_turn(&supervisor.step(Event::Tick,Now(10))),"original one-turn/100ms budget still contains the turn at zero; no turns ran under edited policy");
}

#[test]
fn own_outstanding_turn_posts_must_not_wake_a_moved_bot() {
    let mut supervisor=Supervisor::default();let mut def=definition();def.budget.cooldown_ms=0;def.budget.turns_per_window=10;
    supervisor.step(Event::Define(def.clone()),Now(0));
    let first=turn(&supervisor.step(message(1),Now(1)));
    def.chat=ChatId("chat-new".into());supervisor.step(Event::Define(def.clone()),Now(2));
    supervisor.step(Event::RoomMessage{room:"room".into(),from:Some(ChatId("chat-original".into())),cursor:2},Now(3));
    assert!(!has_turn(&supervisor.step(Event::TurnCompleted{bot:def.bot,turn:first},Now(4))),"only outstanding bot's own post arrived");
}

#[test]
fn overflow_must_not_silently_acknowledge_an_evicted_source() {
    let mut supervisor=Supervisor::default();let mut def=definition();def.budget.cooldown_ms=0;def.budget.turns_per_window=10;
    def.sources=(0..18).map(|n|Source::Service{service:ServiceId(format!("svc-{n}"))}).collect();
    supervisor.step(Event::Define(def.clone()),Now(0));
    let observed=|n,cursor|Event::ResourceObserved{service:ServiceId(format!("svc-{n}")),cursor,kind:"exited".into()};
    let first=turn(&supervisor.step(observed(0,1),Now(1)));
    for n in 1..18 {supervisor.step(observed(n,1),Now(2));}
    let completed=supervisor.step(Event::TurnCompleted{bot:def.bot,turn:first},Now(3));
    let known=completed.iter().find_map(|p|match &p.effect {Effect::RequestTurn{reasons,..}=>Some(reasons),_=>None}).unwrap();
    assert!(known.iter().any(|r|matches!(r,Reason::Service{service,..}if service.0=="svc-1")),"svc-1 exit was silently evicted and its cursor prevents replay");
}
