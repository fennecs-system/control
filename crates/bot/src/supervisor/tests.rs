
use super::*;

/// Test messages: posts that fold, counters that add, exits kept apart.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
enum Msg {
    Post,
    Lines(u64),
    Exit(Option<i32>),
    Kind(String),
}
impl Info for Msg {
    fn urgent(&self) -> bool {
        matches!(self, Self::Exit(_))
    }
    fn merge(&mut self, newer: &Self) -> bool {
        match (self, newer) {
            (Msg::Post, Msg::Post) => true,
            (Msg::Lines(held), Msg::Lines(more)) => {
                *held = held.saturating_add(*more);
                true
            }
            (Msg::Exit(held), Msg::Exit(code)) => {
                *held = *code;
                true
            }
            (Msg::Kind(held), Msg::Kind(kind)) => held == kind,
            _ => false,
        }
    }
}
type S = Supervisor<Msg>;
type R = Reason<Msg>;

fn bot(name: &str) -> BotId {
    BotId(name.into())
}
fn budget(cooldown_ms: u64, window_ms: u64, turns_per_window: u32) -> Budget {
    Budget {
        cooldown_ms,
        window_ms,
        turns_per_window,
    }
}
fn define(s: &mut S, name: &str, sources: Vec<Source>, budget: Budget, now: u64) {
    let definition = Definition {
        bot: bot(name),
        chat: ChatId(format!("chat-{name}")),
        sources,
        budget,
        restart: Restart::Permanent,
    };
    assert!(s.step(Event::Define(definition), Now(now)).is_empty());
}
fn topic(kind: &str, name: &str) -> Topic {
    Topic::new(kind, name)
}
fn room(name: &str) -> Source {
    Source::Topic(topic("room", name))
}
fn message(from: Option<&str>, cursor: u64) -> Event<Msg> {
    Event::Info {
        topic: topic("room", "general"),
        cursor,
        from: from.map(|name| ChatId(format!("chat-{name}"))),
        info: Msg::Post,
    }
}
fn posted(through: u64) -> R {
    Reason::Info {
        topic: topic("room", "general"),
        through,
        info: Msg::Post,
    }
}
fn turns(planned: &[Planned<BotId, Effect<Msg>>]) -> Vec<(&str, TurnId, &[R])> {
    planned
        .iter()
        .filter_map(|p| match &p.effect {
            Effect::RequestTurn {
                bot, turn, reasons, ..
            } => Some((bot.0.as_str(), *turn, reasons.as_slice())),
            _ => None,
        })
        .collect()
}
fn done(name: &str, turn: TurnId) -> Event<Msg> {
    Event::TurnCompleted {
        bot: bot(name),
        turn,
    }
}

#[test]
fn messages_request_turns_coalesce_and_ignore_own_posts_and_replays() {
    let mut s = S::default();
    define(
        &mut s,
        "alice",
        vec![room("general")],
        budget(0, 60_000, 10),
        0,
    );
    define(
        &mut s,
        "cass",
        vec![room("general")],
        budget(0, 60_000, 10),
        0,
    );
    let first = s.step(message(Some("alice"), 1), Now(10));
    let [(name, cass_turn, reasons)] = turns(&first)[..] else {
        panic!("one turn")
    };
    assert_eq!((name, reasons), ("cass", &[posted(1)][..]));
    assert_eq!(first[0].durability, Durability::Intent);
    assert!(matches!(&first[0].effect, Effect::RequestTurn { chat, .. } if chat.0 == "chat-cass"));
    assert_eq!(s.status(&bot("cass")), Some(Status::WaitingTurn));
    let alice = turns(&s.step(message(None, 2), Now(20)))[0].1;
    assert!(
        s.step(message(None, 3), Now(30)).is_empty(),
        "both bots are mid-turn"
    );
    assert!(
        s.step(message(None, 3), Now(31)).is_empty(),
        "replayed cursor"
    );
    assert!(s.step(message(None, 4), Now(40)).is_empty());
    let resumed = s.step(done("cass", cass_turn), Now(50));
    assert_eq!(turns(&resumed)[0].2, [posted(4)]);
    assert!(
        s.step(message(None, 2), Now(60)).is_empty(),
        "an older cursor is a replay"
    );
    assert!(s.eligible(&bot("alice"), &alice));
    assert_eq!(s.outstanding().count(), 2);
}
#[test]
fn active_turns_are_not_restarted_by_callbacks_or_clock_ticks() {
    let mut s = S::default();
    define(
        &mut s,
        "agent",
        vec![room("general")],
        budget(0, 3_600_000, 60),
        0,
    );
    let active = turns(&s.step(message(None, 1), Now(1)))[0].1;
    for cursor in 2..100 {
        assert!(turns(&s.step(message(None, cursor), Now(cursor * 10_000))).is_empty());
        assert!(turns(&s.step(Event::Tick, Now(cursor * 10_000))).is_empty());
        assert!(s.eligible(&bot("agent"), &active));
    }
    s.step(
        Event::Processed {
            bot: bot("agent"),
            topic: topic("room", "general"),
            through: 99,
        },
        Now(1_000_000),
    );
    s.step(Event::Continue(bot("agent")), Now(1_000_001));
    let successor = s.step(done("agent", active), Now(1_000_002));
    assert_eq!(turns(&successor).len(), 1);
    assert_eq!(
        turns(&successor)[0].2,
        [Reason::Continue],
        "processed callbacks do not create another wake"
    );
}
#[test]
fn idle_waits_coalesce_noise_and_resume_on_relevant_events_or_recheck() {
    let mut s = S::default();
    define(
        &mut s,
        "agent",
        vec![room("general"), room("work")],
        budget(0, 60_000, 10),
        0,
    );
    let active = turns(&s.step(message(None, 1), Now(1)))[0].1;
    s.step(
        Event::Wait {
            bot: bot("agent"),
            topics: vec![topic("room", "work")],
            until: Some(10_000),
        },
        Now(2),
    );
    assert!(s.step(done("agent", active), Now(3)).is_empty());
    for cursor in 2..50 {
        assert!(
            s.step(message(None, cursor), Now(cursor * 100)).is_empty(),
            "irrelevant telemetry remains pending without model churn"
        );
    }
    assert_eq!(s.next_deadline(Now(5000)), Some(Now(10_000)));
    let relevant = s.step(
        Event::Info {
            topic: topic("room", "work"),
            cursor: 1,
            from: None,
            info: Msg::Post,
        },
        Now(5001),
    );
    let next = turns(&relevant)[0].1;
    assert_eq!(turns(&relevant).len(), 1);
    s.step(
        Event::Wait {
            bot: bot("agent"),
            topics: vec![],
            until: Some(6000),
        },
        Now(5002),
    );
    s.step(done("agent", next), Now(5003));
    assert!(s.step(Event::Tick, Now(5999)).is_empty());
    let recheck = s.step(Event::Tick, Now(6000));
    assert_eq!(turns(&recheck)[0].2, [Reason::Timer]);
}
#[test]
fn idle_waits_suspend_periodic_checks_but_preserve_deadlines_failures_and_pause() {
    for urgent in [false, true] {
        let mut s = S::default();
        define(
            &mut s,
            "agent",
            vec![room("general"), Source::Timer { every_ms: 100 }],
            budget(0, 60_000, 10),
            0,
        );
        let active = turns(&s.step(message(None, 1), Now(1)))[0].1;
        s.step(
            Event::Wait {
                bot: bot("agent"),
                topics: vec![],
                until: Some(1000),
            },
            Now(2),
        );
        s.step(done("agent", active), Now(3));
        let wake = if urgent {
            s.step(
                Event::Info {
                    topic: topic("room", "general"),
                    cursor: 2,
                    from: None,
                    info: Msg::Exit(Some(1)),
                },
                Now(4),
            )
        } else {
            assert_eq!(s.next_deadline(Now(4)), Some(Now(1000)));
            assert!(s.step(Event::Tick, Now(100)).is_empty());
            s.step(Event::Tick, Now(1000))
        };
        let active = turns(&wake)[0].1;
        s.step(
            Event::Wait {
                bot: bot("agent"),
                topics: vec![],
                until: Some(2000),
            },
            Now(1001),
        );
        s.step(Event::Pause(bot("agent")), Now(1002));
        s.step(done("agent", active), Now(1003));
        assert!(s.step(Event::Tick, Now(3000)).is_empty());
        assert_eq!(s.status(&bot("agent")), Some(Status::Paused));
        assert_eq!(
            turns(&s.step(Event::Resume(bot("agent")), Now(3001))).len(),
            1
        );
    }
}
#[test]
fn event_only_wait_sleeps_for_days_and_retains_new_events_without_preemption() {
    let mut s = S::default();
    define(
        &mut s,
        "agent",
        vec![
            room("general"),
            room("dependency"),
            Source::Timer { every_ms: 100 },
        ],
        budget(0, 60_000, 10),
        0,
    );
    let active = turns(&s.step(message(None, 1), Now(1)))[0].1;
    // A periodic check accumulated while work was active must not defeat wait.
    assert!(s.step(Event::Tick, Now(100)).is_empty());
    s.step(
        Event::Wait {
            bot: bot("agent"),
            topics: vec![topic("room", "dependency")],
            until: None,
        },
        Now(101),
    );
    assert!(s.step(done("agent", active), Now(102)).is_empty());
    #[cfg(feature = "serde")]
    let mut s: S = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    for now in [300_000, 86_400_000, 604_800_000] {
        assert!(s.step(Event::Tick, Now(now)).is_empty());
        assert_eq!(s.next_deadline(Now(now)), None);
    }
    let event = |cursor| Event::Info {
        topic: topic("room", "dependency"),
        cursor,
        from: None,
        info: Msg::Post,
    };
    let wake = s.step(event(1), Now(604_800_001));
    assert_eq!(turns(&wake).len(), 1);
    let active = turns(&wake)[0].1;
    assert!(s.step(event(2), Now(604_800_002)).is_empty());
    assert!(s.eligible(&bot("agent"), &active));
    s.step(
        Event::Processed {
            bot: bot("agent"),
            topic: topic("room", "dependency"),
            through: 1,
        },
        Now(604_800_003),
    );
    s.step(
        Event::Wait {
            bot: bot("agent"),
            topics: vec![topic("room", "dependency")],
            until: None,
        },
        Now(604_800_004),
    );
    assert_eq!(
        turns(&s.step(done("agent", active), Now(604_800_005))).len(),
        1,
        "newer unprocessed callback survives"
    );
    s.step(Event::Stop(bot("agent")), Now(604_800_006));
    assert!(s.step(Event::Tick, Now(700_000_000)).is_empty());
}
#[test]
fn runnable_continuations_coalesce_obey_budget_and_do_not_resurrect_temporary_bots() {
    let mut s = S::default();
    define(
        &mut s,
        "agent",
        vec![room("general")],
        budget(0, 60_000, 2),
        0,
    );
    let first = turns(&s.step(message(None, 1), Now(1)))[0].1;
    assert!(s.step(Event::Continue(bot("agent")), Now(2)).is_empty());
    assert!(s.step(Event::Continue(bot("agent")), Now(3)).is_empty());
    s.step(message(None, 2), Now(4));
    let next = s.step(done("agent", first), Now(5));
    assert_eq!(turns(&next).len(), 1);
    assert_eq!(turns(&next)[0].2, [Reason::Continue, posted(2)]);
    let second = turns(&next)[0].1;
    s.step(Event::Continue(bot("agent")), Now(6));
    assert!(
        turns(&s.step(done("agent", second), Now(7))).is_empty(),
        "continuations cannot bypass the two-turn budget"
    );
    assert_eq!(turns(&s.step(Event::Tick, Now(60_001))).len(), 1);
    s.step(Event::Stop(bot("agent")), Now(60_002));
    assert!(turns(&s.step(Event::Continue(bot("agent")), Now(60_003))).is_empty());

    let mut once = S::default();
    once.step(
        Event::Define(Definition {
            bot: bot("once"),
            chat: ChatId("once-chat".into()),
            sources: vec![room("general")],
            budget: budget(0, 60_000, 10),
            restart: Restart::Temporary,
        }),
        Now(0),
    );
    let first = turns(&once.step(message(None, 1), Now(1)))[0].1;
    once.step(Event::Continue(bot("once")), Now(2));
    assert!(once.step(done("once", first), Now(3)).is_empty());
    assert_eq!(
        once.status(&bot("once")),
        None,
        "temporary callbacks still need explicit rearm"
    );
}
#[test]
fn published_processing_can_precede_supervisor_ingestion_without_replaying_it() {
    let mut s = S::default();
    define(
        &mut s,
        "agent",
        vec![room("general")],
        budget(0, 60_000, 10),
        0,
    );
    s.step(
        Event::Processed {
            bot: bot("agent"),
            topic: topic("room", "general"),
            through: 8,
        },
        Now(1),
    );
    for cursor in 1..=8 {
        assert!(s.step(message(None, cursor), Now(cursor + 1)).is_empty());
    }
    assert_eq!(turns(&s.step(message(None, 9), Now(10)))[0].2, [posted(9)]);
}
#[test]
fn committed_processing_retires_only_matching_pending_watermarks() {
    for (newest, expect_next) in [(2, false), (3, true)] {
        let mut s = S::default();
        define(
            &mut s,
            "agent",
            vec![room("general")],
            budget(0, 60_000, 10),
            0,
        );
        let first = turns(&s.step(message(None, 1), Now(1)))[0].1;
        s.step(message(None, 2), Now(2));
        if newest == 3 {
            s.step(message(None, 3), Now(3));
        }
        s.step(
            Event::Processed {
                bot: bot("agent"),
                topic: topic("room", "general"),
                through: 2,
            },
            Now(4),
        );
        let next = s.step(done("agent", first), Now(5));
        assert_eq!(
            !turns(&next).is_empty(),
            expect_next,
            "newer events survive a racing acknowledgement"
        );
        if expect_next {
            assert_eq!(turns(&next)[0].2, [posted(3)]);
        }
    }
}

#[test]
fn temporary_bots_end_after_their_first_turn_and_redefinition_can_make_them_permanent() {
    let mut s = S::default();
    let once = Definition {
        bot: bot("once"),
        chat: ChatId("chat-once".into()),
        sources: vec![room("general")],
        budget: budget(0, 60_000, 1),
        restart: Restart::Temporary,
    };
    assert!(s.step(Event::Define(once.clone()), Now(0)).is_empty());
    let turn = turns(&s.step(message(None, 1), Now(1)))[0].1;
    assert!(s.step(message(None, 2), Now(2)).is_empty());
    assert!(
        s.step(done("once", turn), Now(3)).is_empty(),
        "pending messages after the wake are not a second turn"
    );
    assert_eq!(s.status(&bot("once")), None);
    s.step(Event::Define(once.clone()), Now(4));
    s.step(
        Event::Define(Definition {
            restart: Restart::Permanent,
            ..once
        }),
        Now(5),
    );
    let turn = turns(&s.step(message(None, 3), Now(6)))[0].1;
    s.step(done("once", turn), Now(7));
    assert_eq!(s.status(&bot("once")), Some(Status::Ready));
}

#[test]
fn stale_or_duplicate_completions_never_release_a_newer_turn() {
    let mut s = S::default();
    define(
        &mut s,
        "bot",
        vec![room("general")],
        budget(0, 60_000, 10),
        0,
    );
    let first = turns(&s.step(message(None, 1), Now(1)))[0].1;
    assert!(s.step(message(None, 2), Now(2)).is_empty());
    let second = turns(&s.step(done("bot", first), Now(3)))[0].1;
    assert_ne!(first, second);
    assert!(s.step(message(None, 3), Now(4)).is_empty());
    assert!(
        s.step(done("bot", first), Now(5)).is_empty(),
        "a duplicate completion is stale"
    );
    assert_eq!(s.status(&bot("bot")), Some(Status::WaitingTurn));
    assert!(!s.eligible(&bot("bot"), &first) && s.eligible(&bot("bot"), &second));
    assert_eq!(turns(&s.step(done("bot", second), Now(6))).len(), 1);
}

#[test]
fn explicit_rearm_keeps_exactly_one_next_callback_and_stop_still_wins() {
    let mut s = S::default();
    let once = Definition {
        bot: bot("once"),
        chat: ChatId("chat-once".into()),
        sources: vec![room("general")],
        budget: budget(0, 60_000, 1),
        restart: Restart::Temporary,
    };
    s.step(Event::Define(once.clone()), Now(0));
    let first = turns(&s.step(message(None, 1), Now(1)))[0].1;
    assert!(s.retains(&Topic::new("room", "general")));
    s.step(Event::Rearm(bot("once")), Now(2));
    s.step(Event::Rearm(bot("once")), Now(2));
    assert!(s.step(message(None, 2), Now(3)).is_empty());
    let effects = s.step(done("once", first), Now(4));
    let next = turns(&effects);
    assert_eq!(
        next.len(),
        1,
        "an explicit request survives the previous callback"
    );
    assert!(s.step(done("once", next[0].1), Now(5)).is_empty());
    assert_eq!(s.status(&bot("once")), None);
    assert!(!s.retains(&Topic::new("room", "general")));

    s.step(Event::Define(once), Now(6));
    let turn = turns(&s.step(message(None, 3), Now(7)))[0].1;
    s.step(Event::Rearm(bot("once")), Now(8));
    s.step(Event::Stop(bot("once")), Now(9));
    s.step(done("once", turn), Now(10));
    assert_eq!(s.status(&bot("once")), None);
}

#[test]
fn a_paused_temporary_callback_keeps_its_mailbox_until_explicit_resume() {
    let mut s = S::default();
    s.step(
        Event::Define(Definition {
            bot: bot("once"),
            chat: ChatId("chat-once".into()),
            sources: vec![room("general")],
            budget: budget(0, 60_000, 1),
            restart: Restart::Temporary,
        }),
        Now(0),
    );
    let first = turns(&s.step(message(None, 1), Now(1)))[0].1;
    s.step(Event::Pause(bot("once")), Now(2));
    s.step(message(None, 2), Now(3));
    assert!(s.step(done("once", first), Now(4)).is_empty());
    assert_eq!(s.status(&bot("once")), Some(Status::Paused));
    assert_eq!(turns(&s.step(Event::Resume(bot("once")), Now(5))).len(), 1);
}

#[test]
fn budgets_bound_turns_survive_redefinition_and_report_exhaustion_once() {
    let mut s = S::default();
    define(
        &mut s,
        "bot",
        vec![room("general")],
        budget(1_000, 10_000, 2),
        0,
    );
    let t1 = turns(&s.step(message(None, 1), Now(0)))[0].1;
    s.step(done("bot", t1), Now(100));
    assert!(s.step(message(None, 2), Now(200)).is_empty(), "cooldown");
    assert_eq!(s.next_deadline(Now(200)), Some(Now(1_000)));
    let t2 = turns(&s.step(Event::Tick, Now(1_000)))[0].1;
    s.step(done("bot", t2), Now(1_100));
    define(
        &mut s,
        "bot",
        vec![room("general")],
        budget(1_000, 10_000, 2),
        1_200,
    );
    let exhausted = s.step(message(None, 3), Now(2_000));
    assert!(
        matches!(
            &exhausted[..],
            [Planned {
                effect: Effect::BudgetExhausted { until: 10_000, .. },
                durability: Durability::None,
                ..
            }]
        ),
        "redefinition keeps budget accounting"
    );
    assert!(
        s.step(message(None, 4), Now(3_000)).is_empty(),
        "exhaustion is reported once per window"
    );
    assert_eq!(s.next_deadline(Now(3_000)), Some(Now(10_000)));
    assert_eq!(turns(&s.step(Event::Tick, Now(10_000)))[0].2, [posted(4)]);
}

#[test]
fn timers_wake_idle_bots_and_set_the_next_deadline() {
    let mut s = S::default();
    define(
        &mut s,
        "scout",
        vec![Source::Timer { every_ms: 5_000 }],
        budget(0, 60_000, 100),
        1_000,
    );
    assert_eq!(s.next_deadline(Now(1_000)), Some(Now(6_000)));
    assert!(s.step(Event::Tick, Now(5_999)).is_empty());
    let turn = turns(&s.step(Event::Tick, Now(6_000)))[0].1;
    assert!(
        s.step(Event::Tick, Now(11_000)).is_empty(),
        "a running turn defers the timer"
    );
    assert_eq!(
        turns(&s.step(done("scout", turn), Now(11_500)))[0].2,
        [Reason::Timer]
    );
    assert_eq!(s.next_deadline(Now(11_500)), Some(Now(16_000)));
}

#[test]
fn deadlines_beyond_the_clock_never_fire_or_loop() {
    let mut s = S::default();
    define(
        &mut s,
        "late",
        vec![Source::Timer { every_ms: 10 }],
        budget(0, 60_000, 10),
        u64::MAX - 5,
    );
    assert_eq!(
        s.next_deadline(Now(u64::MAX - 5)),
        None,
        "the first timer is unrepresentable"
    );
    assert!(s.step(Event::Tick, Now(u64::MAX)).is_empty());
    define(
        &mut s,
        "edge",
        vec![room("general")],
        budget(10, u64::MAX, 1),
        0,
    );
    let turn = turns(&s.step(message(None, 1), Now(u64::MAX - 1)))[0].1;
    s.step(done("edge", turn), Now(u64::MAX));
    let blocked = s.step(message(None, 2), Now(u64::MAX));
    assert!(matches!(
        &blocked[..],
        [Planned {
            effect: Effect::BudgetExhausted {
                until: u64::MAX,
                ..
            },
            ..
        }]
    ));
    assert!(s.step(Event::Tick, Now(u64::MAX)).is_empty());
    assert_eq!(s.next_deadline(Now(u64::MAX)), None);
}

#[test]
fn messages_reach_only_subscribers_and_overflow_names_topics_to_resync() {
    let mut s = S::default();
    let service = |name: &str| topic("service", name);
    let observed = |name: &str, cursor: u64, kind: &str| Event::Info {
        topic: service(name),
        cursor,
        from: None,
        info: Msg::Kind(kind.into()),
    };
    define(
        &mut s,
        "alice",
        vec![Source::Topic(service("svc-alice"))],
        budget(0, 60_000, 10),
        0,
    );
    let turn = turns(&s.step(observed("svc-alice", 1, "started"), Now(0)))[0].1;
    assert!(s.step(observed("svc-cass", 2, "died"), Now(1)).is_empty());
    for cursor in 2..42 {
        assert!(
            s.step(
                observed("svc-alice", cursor, &format!("event-{}", cursor % 20)),
                Now(2)
            )
            .is_empty()
        );
    }
    assert!(
        s.step(observed("svc-alice", 41, "replayed"), Now(3))
            .is_empty()
    );
    let woke = s.step(done("alice", turn), Now(4));
    let (second, reasons) = (turns(&woke)[0].1, turns(&woke)[0].2.to_vec());
    assert_eq!(
        reasons.len(),
        PENDING + 1,
        "the first sixteen distinct reasons plus an overflow marker"
    );
    assert_eq!(
        reasons.last(),
        Some(&Reason::Overflow {
            dropped: 8,
            topics: vec![service("svc-alice")]
        })
    );
    assert!(
        reasons[..PENDING]
            .iter()
            .all(|r| matches!(r, Reason::Info { topic, .. } if topic.name == "svc-alice"))
    );
    assert!(
        !reasons
            .iter()
            .any(|r| matches!(r, Reason::Info { info: Msg::Kind(kind), .. } if kind == "replayed"))
    );
    assert!(
        s.step(observed("svc-alice", 18, "event-18"), Now(5))
            .is_empty(),
        "dropped messages are acknowledged, not replayed"
    );
    assert!(s.step(done("alice", second), Now(6)).is_empty());
}

#[test]
fn application_merge_rules_decide_coalescing_per_topic() {
    let mut s = S::default();
    let watch = |name: &str| topic("watch", name);
    let info = |name: &str, cursor: u64, info: Msg| Event::Info {
        topic: watch(name),
        cursor,
        from: None,
        info,
    };
    define(
        &mut s,
        "alice",
        vec![Source::Topic(watch("w-alice"))],
        budget(0, 60_000, 10),
        0,
    );
    assert!(
        s.step(info("w-cass", 5, Msg::Lines(3)), Now(1)).is_empty(),
        "other topics are not this bot's messages"
    );
    let woke = s.step(info("w-alice", 10, Msg::Lines(2)), Now(2));
    let (_, turn, reasons) = turns(&woke)[0];
    assert_eq!(
        reasons,
        [Reason::Info {
            topic: watch("w-alice"),
            through: 10,
            info: Msg::Lines(2)
        }]
    );
    assert!(
        s.step(info("w-alice", 15, Msg::Lines(4)), Now(3))
            .is_empty()
    );
    assert!(
        s.step(info("w-alice", 15, Msg::Lines(4)), Now(3))
            .is_empty(),
        "replays at or below the cursor are ignored"
    );
    assert!(
        s.step(info("w-alice", 20, Msg::Lines(1)), Now(4))
            .is_empty()
    );
    assert!(
        s.step(info("w-alice", 21, Msg::Exit(Some(137))), Now(5))
            .is_empty()
    );
    assert!(
        s.step(info("w-alice", 22, Msg::Exit(Some(0))), Now(5))
            .is_empty()
    );
    let next = s.step(done("alice", turn), Now(6));
    let (_, _, reasons) = turns(&next)[0];
    assert_eq!(
        reasons,
        [
            Reason::Info {
                topic: watch("w-alice"),
                through: 20,
                info: Msg::Lines(5)
            },
            Reason::Info {
                topic: watch("w-alice"),
                through: 22,
                info: Msg::Exit(Some(0))
            },
        ],
        "lines add up; exits fold separately with the latest code"
    );
}

#[test]
fn topics_with_colons_keep_separate_cursors() {
    let mut s = S::default();
    let (left, right) = (topic("a:b", "c"), topic("a", "b:c"));
    define(
        &mut s,
        "bot",
        vec![Source::Topic(left.clone()), Source::Topic(right.clone())],
        budget(0, 60_000, 10),
        0,
    );
    let turn = turns(&s.step(
        Event::Info {
            topic: left,
            cursor: 5,
            from: None,
            info: Msg::Post,
        },
        Now(1),
    ))[0]
        .1;
    assert!(
        s.step(
            Event::Info {
                topic: right.clone(),
                cursor: 3,
                from: None,
                info: Msg::Post
            },
            Now(2)
        )
        .is_empty()
    );
    let next = s.step(done("bot", turn), Now(3));
    assert_eq!(
        turns(&next)[0].2,
        [Reason::Info {
            topic: right,
            through: 3,
            info: Msg::Post
        }]
    );
}

#[test]
fn pause_stop_and_validation_follow_the_command_contract() {
    let mut s = S::default();
    for definition in [
        Definition {
            bot: bot("x"),
            chat: ChatId("c".into()),
            sources: vec![],
            budget: budget(0, 1, 1),
            restart: Restart::Permanent,
        },
        Definition {
            bot: bot("x"),
            chat: ChatId("c".into()),
            sources: vec![room("general")],
            budget: budget(0, 0, 1),
            restart: Restart::Permanent,
        },
        Definition {
            bot: bot("x"),
            chat: ChatId("c".into()),
            sources: vec![Source::Timer { every_ms: 0 }],
            budget: budget(0, 1, 1),
            restart: Restart::Permanent,
        },
        Definition {
            bot: bot("x"),
            chat: ChatId("c".into()),
            sources: vec![room("general")],
            budget: budget(0, 1, 257),
            restart: Restart::Permanent,
        },
    ] {
        assert!(definition.validate().is_err());
        assert!(s.step(Event::Define(definition), Now(0)).is_empty());
    }
    assert_eq!(s.definitions().count(), 0);
    define(
        &mut s,
        "bot",
        vec![room("general")],
        budget(0, 60_000, 10),
        0,
    );
    assert!(s.step(Event::Pause(bot("bot")), Now(1)).is_empty());
    assert!(s.step(message(None, 1), Now(2)).is_empty());
    assert_eq!(s.status(&bot("bot")), Some(Status::Paused));
    let turn = turns(&s.step(Event::Resume(bot("bot")), Now(3)))[0].1;
    assert!(s.step(Event::Stop(bot("bot")), Now(4)).is_empty());
    assert_eq!(
        s.status(&bot("bot")),
        Some(Status::Stopping),
        "stop waits for the running turn"
    );
    assert!(
        !s.eligible(&bot("bot"), &turn),
        "a stopping bot's turn is not re-queued"
    );
    define(
        &mut s,
        "bot",
        vec![room("general")],
        budget(0, 60_000, 10),
        5,
    );
    assert!(
        s.step(message(None, 2), Now(6)).is_empty(),
        "a stopping bot admits nothing"
    );
    assert!(s.step(done("bot", turn), Now(7)).is_empty());
    assert_eq!(s.status(&bot("bot")), None);
    assert_eq!(s.next_deadline(Now(8)), None);
}

#[cfg(feature = "serde")]
#[test]
fn restored_state_round_trips_and_definitions_outside_the_contract_never_produce_turns() {
    let mut s = S::default();
    define(
        &mut s,
        "bot",
        vec![room("general")],
        budget(0, 60_000, 10),
        0,
    );
    let turn = turns(&s.step(message(None, 1), Now(1)))[0].1;
    for i in 0..20 {
        s.step(
            Event::Info {
                topic: topic("watch", &format!("w{i}")),
                cursor: 1,
                from: None,
                info: Msg::Exit(None),
            },
            Now(2),
        );
    }
    s.step(message(None, 2), Now(2));
    let mut restored: S = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
    assert_eq!(
        turns(&restored.step(done("bot", turn), Now(3)))[0].2,
        [posted(2)],
        "only subscribed topics were admitted"
    );
    let mut snapshot = serde_json::to_value(&restored).unwrap();
    snapshot["bots"]["bot"]["definition"]["budget"]["turns_per_window"] = 1000.into();
    let mut invalid: S = serde_json::from_value(snapshot).unwrap();
    assert!(
        turns(&invalid.step(message(None, 9), Now(9))).is_empty(),
        "an invalid restored budget fails closed"
    );
    assert_eq!(invalid.next_deadline(Now(9)), None);
}

#[test]
fn outstanding_turns_keep_their_destination_stops_clear_timers_and_cursor_zero_counts() {
    let mut s = S::default();
    define(
        &mut s,
        "bot",
        vec![room("general"), Source::Timer { every_ms: 100 }],
        budget(0, 60_000, 10),
        0,
    );
    let turn = turns(&s.step(message(None, 0), Now(1)))[0].1;
    let moved = Definition {
        bot: bot("bot"),
        chat: ChatId("chat-moved".into()),
        sources: vec![room("general"), Source::Timer { every_ms: 100 }],
        budget: budget(0, 60_000, 10),
        restart: Restart::Permanent,
    };
    assert!(s.step(Event::Define(moved), Now(2)).is_empty());
    assert_eq!(
        s.outstanding()
            .map(|(_, chat, t)| (chat.0.as_str(), *t))
            .collect::<Vec<_>>(),
        [("chat-bot", turn)],
        "recovery uses the chat the turn was requested for"
    );
    assert!(s.step(Event::Stop(bot("bot")), Now(3)).is_empty());
    assert_eq!(
        s.next_deadline(Now(500)),
        None,
        "a stopping bot leaves no timer deadline"
    );
    assert!(s.step(Event::Tick, Now(500)).is_empty());
    assert!(s.step(done("bot", turn), Now(501)).is_empty());
    assert_eq!(s.status(&bot("bot")), None);
}

#[test]
fn pause_intent_is_observable_without_discarding_an_outstanding_turn() {
    let mut s = S::default();
    define(&mut s, "bot", vec![room("general")], budget(0, 60_000, 10), 0);
    let turn = turns(&s.step(message(None, 1), Now(1)))[0].1;
    assert!(!s.paused(&bot("bot")));
    s.step(Event::Pause(bot("bot")), Now(2));
    let mut restored: S = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert!(restored.paused(&bot("bot")));
    assert_eq!(restored.status(&bot("bot")), Some(Status::WaitingTurn));
    assert!(restored.eligible(&bot("bot"), &turn));
    assert!(!restored.may_start(&bot("bot"), &turn));
    restored.step(Event::Resume(bot("bot")), Now(3));
    assert!(!restored.paused(&bot("bot")));
    assert!(restored.may_start(&bot("bot"), &turn));
}
