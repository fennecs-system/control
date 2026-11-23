use super::*;

/// A stand-in backend: accounts know backends only through the trait.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Test {
    Codex,
    Claude,
}
impl Backend for Test {
    fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
    fn implicit(self) -> (AccountKind, SecretRef) {
        let file = match self {
            Self::Codex => "codex/auth.json",
            Self::Claude => "claude/.credentials.json",
        };
        (
            AccountKind::Subscription,
            SecretRef(vec![Location::Config(RelativePath::try_from(file).unwrap())]),
        )
    }
    fn login_file(self) -> Option<&'static str> {
        Some(match self {
            Self::Codex => "auth.json",
            Self::Claude => ".credentials.json",
        })
    }
    fn key_file(self) -> Option<KeyFile> {
        match self {
            Self::Codex => Some(KeyFile {
                name: "auth.json",
                writer: KeyWriter::Cli,
            }),
            Self::Claude => None,
        }
    }
    fn signs_in(self) -> bool {
        true
    }
}
fn name(text: &str) -> AccountName {
    AccountName::try_from(text).unwrap()
}
const EXAMPLE: &str = r#"{
  "defaults": {"codex": "work"},
  "accounts": [
    {"backend": "codex", "name": "work", "kind": "subscription", "cap": 2, "label": "Work plan"},
    {"backend": "codex", "name": "key", "kind": "api_key", "enabled": false},
    {"backend": "codex", "name": "second", "kind": "subscription"}
  ]
}"#;

#[test]
fn names_are_lower_case_bounded_and_default_is_the_implicit_account() {
    assert_eq!(AccountName::try_from(""), Err(NameError::Empty));
    assert_eq!(AccountName::try_from("a".repeat(33).as_str()), Err(NameError::TooLong));
    assert_eq!(AccountName::try_from("Second"), Err(NameError::Character('S')));
    assert_eq!(AccountName::try_from("a+b"), Err(NameError::Character('+')));
    assert_eq!(AccountName::try_from("*"), Err(NameError::Character('*')));
    assert!(AccountName::try_from("a".repeat(32).as_str()).is_ok());
    assert!(name("default").is_implicit());
    assert_eq!(AccountName::default(), AccountName::implicit());
    assert!(!name("work-2").is_implicit());
}

#[test]
fn a_choice_is_saved_as_an_optional_string() {
    for (choice, saved) in [
        (AccountChoice::Named(name("second")), r#""second""#),
        (AccountChoice::Pool, r#""*""#),
    ] {
        assert_eq!(serde_json::to_string(&choice).unwrap(), saved);
        assert_eq!(serde_json::from_str::<AccountChoice>(saved).unwrap(), choice);
    }
    assert_eq!(
        serde_json::from_str::<AccountChoice>("null").unwrap(),
        AccountChoice::Default
    );
    assert!(serde_json::from_str::<AccountChoice>(r#""Bad""#).is_err());
    assert_eq!(AccountChoice::Default.suffix(), None);
    assert_eq!(AccountChoice::Named(AccountName::implicit()).suffix(), None);
    assert_eq!(AccountChoice::Pool.suffix(), Some("*"));
}

#[test]
fn without_a_file_every_backend_has_only_its_implicit_account() {
    let store = Store::<Test>::default();
    let implicit = default_for(Test::Claude);
    assert_eq!(implicit.id.name, AccountName::implicit());
    assert!(implicit.enabled);
    assert_eq!(store.of(Test::Claude).collect::<Vec<_>>(), vec![implicit.clone()]);
    assert_eq!(store.default_name(Test::Claude), AccountName::implicit());
    for choice in [
        AccountChoice::Default,
        AccountChoice::Pool,
        AccountName::implicit().into(),
    ] {
        assert_eq!(
            store.choose(Test::Claude, &choice, |_| 0, |_| false),
            Ok(implicit.clone())
        );
    }
    let missing = store
        .choose(Test::Claude, &name("second").into(), |_| 0, |_| false)
        .unwrap_err();
    assert_eq!(
        (missing.why, missing.existing),
        (Unusable::Missing(name("second")), vec![AccountName::implicit()])
    );
}

#[test]
fn a_hand_written_file_defines_named_accounts_and_defaults() {
    let store = Store::<Test>::parse(EXAMPLE.as_bytes()).unwrap();
    assert_eq!(store.default_name(Test::Codex), name("work"));
    assert_eq!(store.default_name(Test::Claude), AccountName::implicit());
    let work = store.get(Test::Codex, &name("work")).unwrap();
    assert_eq!(work.cap, NonZeroU8::new(2));
    assert_eq!(work.label, Some(Label::try_from("Work plan".to_owned()).unwrap()));
    assert_eq!(
        work.secret,
        SecretRef(vec![Location::Config(
            RelativePath::try_from("accounts/codex/work/auth.json").unwrap()
        )])
    );
    let key = store.get(Test::Codex, &name("key")).unwrap();
    assert_eq!((key.kind, key.enabled), (AccountKind::ApiKey, false));
    let names = |backend| store.of(backend).map(|a| a.id.name.to_string()).collect::<Vec<_>>();
    assert_eq!(names(Test::Codex), ["default", "work", "key", "second"]);
    assert_eq!(names(Test::Claude), ["default"]);

    assert_eq!(
        store
            .choose(Test::Codex, &AccountChoice::Default, |_| 0, |_| false)
            .unwrap()
            .id
            .name,
        name("work")
    );
    let disabled = store
        .choose(Test::Codex, &name("key").into(), |_| 0, |_| false)
        .unwrap_err();
    assert_eq!(disabled.why, Unusable::Disabled(name("key")));
    assert_eq!(disabled.existing.len(), 4);
    // A pool skips disabled accounts and takes the least loaded, the first on a tie.
    let load = |busy: &'static [&'static str]| move |n: &AccountName| usize::from(busy.contains(&n.as_str()));
    let pooled = |busy| {
        store
            .choose(Test::Codex, &AccountChoice::Pool, load(busy), |_| false)
            .unwrap()
            .id
            .name
    };
    assert_eq!(pooled(&[]), AccountName::implicit());
    assert_eq!(pooled(&["default"]), name("work"));
    assert_eq!(pooled(&["default", "work"]), name("second"));
    assert_eq!(pooled(&["default", "work", "second"]), AccountName::implicit());
}

#[test]
fn invalid_files_are_refused_whole() {
    let refused = |text: &str| Store::<Test>::parse(text.as_bytes()).unwrap_err().to_string();
    let account = |name: &str| format!(r#"{{"backend":"codex","name":"{name}","kind":"subscription"}}"#);
    assert!(refused(&format!(r#"{{"accounts":[{}]}}"#, account("default"))).contains("implicit account"));
    assert!(refused(&format!(r#"{{"accounts":[{0},{0}]}}"#, account("two"))).contains("twice"));
    assert!(refused(r#"{"defaults":{"codex":"nobody"}}"#).contains("does not define"));
    assert!(refused(r#"{"defaults":{"codex":"default","codex":"default"}}"#).contains("twice"));
    let disabled = r#"{"defaults":{"codex":"off"},"accounts":[{"backend":"codex","name":"off","kind":"api_key","enabled":false}]}"#;
    assert!(refused(disabled).contains("enabled"));
    for invalid in [
        r#"{"accounts":[{"backend":"grok","name":"a","kind":"subscription"}]}"#,
        r#"{"accounts":[{"backend":"codex","name":"A","kind":"subscription"}]}"#,
        r#"{"accounts":[{"backend":"codex","name":"a","kind":"plan"}]}"#,
        r#"{"accounts":[{"backend":"codex","name":"a","kind":"subscription","cap":0}]}"#,
        // A login's location follows from the account; it is not written down.
        r#"{"accounts":[{"backend":"codex","name":"a","kind":"subscription","secret":{"config":"a"}}]}"#,
        r#"{"accounts":[{"backend":"codex","name":"a","kind":"subscription","extra":1}]}"#,
        r#"{"unknown":{}}"#,
    ] {
        assert!(refused(invalid).starts_with("accounts.json is invalid"), "{invalid}");
    }
}

#[test]
fn the_reader_treats_a_missing_file_as_no_accounts() {
    let directory = std::env::temp_dir().join(format!("control-accounts-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let _ = std::fs::remove_file(directory.join("accounts.json"));
    assert_eq!(read::<Test>(&directory).unwrap(), Store::default());
    std::fs::write(directory.join("accounts.json"), EXAMPLE).unwrap();
    assert_eq!(
        read::<Test>(&directory).unwrap().default_name(Test::Codex),
        name("work")
    );
    std::fs::write(directory.join("accounts.json"), "{").unwrap();
    assert!(read::<Test>(&directory).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_reference_resolves_to_its_first_available_location() {
    let secret = SecretRef(vec![
        Location::Variable {
            name: "OVERRIDE",
            join: None,
        },
        Location::Variable {
            name: "TOOL_HOME",
            join: Some(RelativePath::try_from("auth.json").unwrap()),
        },
        Location::Home(RelativePath::try_from(".tool/auth.json").unwrap()),
        Location::Config(RelativePath::try_from("tool/auth.json").unwrap()),
    ]);
    let config = Path::new("/config");
    let resolve = |set: &[(&str, &str)], home: Option<&Path>| {
        let variable = |name: &str| set.iter().find(|(n, _)| *n == name).map(|(_, v)| PathBuf::from(v));
        secret
            .resolve(&Roots {
                config,
                home,
                variable: &variable,
            })
            .map(|(_, path)| path)
    };
    let home = Some(Path::new("/home/op"));
    assert_eq!(resolve(&[], None), Some(PathBuf::from("/config/tool/auth.json")));
    assert_eq!(resolve(&[], home), Some(PathBuf::from("/home/op/.tool/auth.json")));
    assert_eq!(
        resolve(&[("TOOL_HOME", "/t")], home),
        Some(PathBuf::from("/t/auth.json"))
    );
    let both = [("TOOL_HOME", "/t"), ("OVERRIDE", "/o/file")];
    assert_eq!(resolve(&both, home), Some(PathBuf::from("/o/file")));
    assert_eq!(
        SecretRef(Vec::new()).resolve(&Roots {
            config,
            home,
            variable: &|_| None
        }),
        None
    );
}

#[test]
fn a_named_accounts_login_is_in_its_own_directory() {
    assert_eq!(
        secret(Test::Claude, &name("second"), AccountKind::Subscription),
        SecretRef(vec![Location::Config(
            RelativePath::try_from("accounts/claude/second/.credentials.json").unwrap()
        )])
    );
    assert_eq!(
        secret(Test::Claude, &AccountName::implicit(), AccountKind::Subscription),
        Test::Claude.implicit().1
    );
    assert_eq!(
        directory(Test::Codex, &name("second")).as_path(),
        Path::new("accounts/codex/second")
    );
    // No account is capped unless the operator sets one: a named account runs
    // as many attempts as the implicit one always has.
    let store = Store::<Test>::parse(EXAMPLE.as_bytes()).unwrap();
    let cap = |backend, account: &str| store.get(backend, &name(account)).unwrap().cap;
    assert_eq!(cap(Test::Codex, "second"), None);
    assert_eq!(
        cap(Test::Codex, "work"),
        NonZeroU8::new(2),
        "the operator's cap applies"
    );
    assert_eq!(cap(Test::Codex, "default"), None);
}

#[test]
fn edits_are_validated_and_saved_in_the_format_read() {
    let entry = |backend, account: &str| Entry {
        backend,
        name: name(account),
        kind: AccountKind::Subscription,
        cap: None,
        enabled: true,
        label: None,
    };
    let store = Store::<Test>::default().add(entry(Test::Claude, "second")).unwrap();
    let store = store.set_default(Test::Claude, &name("second")).unwrap();
    assert_eq!(store.default_name(Test::Claude), name("second"));
    let saved = String::from_utf8(store.encode()).unwrap();
    assert!(saved.contains(r#""claude": "second""#), "{saved}");
    assert_eq!(Store::<Test>::parse(saved.as_bytes()).unwrap(), store);
    assert_eq!(
        Store::<Test>::parse(EXAMPLE.as_bytes())
            .map(|s| Store::<Test>::parse(&s.encode()).unwrap())
            .unwrap(),
        Store::<Test>::parse(EXAMPLE.as_bytes()).unwrap()
    );
    let refused = |result: Result<Store<Test>, LoadError<Test>>| result.unwrap_err().to_string();
    assert!(refused(store.add(entry(Test::Claude, "second"))).contains("twice"));
    assert!(refused(store.add(entry(Test::Claude, "default"))).contains("implicit"));
    assert!(refused(store.set_default(Test::Codex, &name("nobody"))).contains("does not define"));
    assert!(refused(store.remove(Test::Codex, &name("nobody"))).contains("has no account nobody"));
    // Removing the default account restores the implicit one.
    let removed = store.remove(Test::Claude, &name("second")).unwrap();
    assert_eq!(removed, Store::default());
    let restored = store.set_default(Test::Claude, &AccountName::implicit()).unwrap();
    assert_eq!(restored.default_name(Test::Claude), AccountName::implicit());
}

#[test]
fn a_usage_limit_cools_an_account_and_admission_holds_it_without_failover() {
    let store = Store::<Test>::parse(EXAMPLE.as_bytes()).unwrap();
    let second = store.get(Test::Codex, &name("second")).unwrap();
    let now = UnixSeconds(1_000);
    let status = Status::<Test>::default();
    assert_eq!(may_start(&second, &status, 0, now), Ok(()));
    assert_eq!(
        may_start(&second, &status, 5, now),
        Ok(()),
        "uncapped unless the operator says so"
    );
    let work = store.get(Test::Codex, &name("work")).unwrap();
    assert_eq!(may_start(&work, &status, 1, now), Ok(()));
    assert_eq!(
        may_start(&work, &status, 2, now),
        Err(Hold::Cap(NonZeroU8::new(2).unwrap()))
    );
    let limited = |status: &Status<Test>, started, at, retry| {
        status.ended(
            Test::Codex,
            &name("second"),
            UnixSeconds(started),
            Ended::Limited(retry),
            UnixSeconds(at),
        )
    };
    // No stated time: held for the first backoff step, never until cleared.
    let cooled = limited(&status, 990, 1_000, None);
    assert_eq!(
        may_start(&second, &cooled, 0, UnixSeconds(1_299)),
        Err(Hold::Cooling(RetryAt(UnixSeconds(1_300))))
    );
    // Another attempt that began before the retry time hit the same limit:
    // it keeps when the limit began and the step, and a later stated time wins.
    let again = limited(&cooled, 995, 1_100, Some(StatedRetry::On(3_000)));
    let cooling = again.cooling(Test::Codex, &name("second")).unwrap();
    assert_eq!(
        (cooling.since, cooling.until, cooling.strikes),
        (now, RetryAt(UnixSeconds(3_000)), Strikes::FIRST)
    );
    assert_eq!(cooled.clear(Test::Codex, &name("second")), status);
    // A pool skips a cooling account while another is available.
    let cooling = |busy: &'static str| move |n: &AccountName| n.as_str() == busy;
    let pooled = |cool| {
        store
            .choose(Test::Codex, &AccountChoice::Pool, |_| 0, cooling(cool))
            .unwrap()
            .id
            .name
    };
    assert_eq!(pooled("default"), name("work"));
    assert_eq!(pooled("nobody"), AccountName::implicit());
    // A named choice is never moved to another account.
    let named = store
        .choose(Test::Codex, &name("second").into(), |_| 0, |_| true)
        .unwrap();
    assert_eq!(named.id.name, name("second"));
    // Saved with the node state, and absent when nothing cools.
    assert_eq!(serde_json::to_string(&status).unwrap(), "{}");
    let saved = serde_json::to_string(&cooled).unwrap();
    assert_eq!(
        saved,
        r#"{"cooling":[{"backend":"codex","name":"second","since":1000,"until":1300,"strikes":1}]}"#
    );
    assert_eq!(serde_json::from_str::<Status<Test>>(&saved).unwrap(), cooled);
}

/// Without a stated time each limit in a row waits longer, up to an hour.
#[test]
fn backoff_steps_from_five_minutes_to_an_hour() {
    let minutes = |strikes: u8| backoff(Strikes(NonZeroU8::new(strikes).unwrap())).as_secs() / 60;
    let table = [(1, 5), (2, 15), (3, 60), (4, 60), (255, 60)];
    assert_eq!(table.map(|(strikes, _)| (strikes, minutes(strikes))), table);
    assert_eq!(Strikes(NonZeroU8::MAX).next(), Strikes(NonZeroU8::MAX), "saturates");
    assert_eq!(BACKOFF_MAX.as_secs(), 3_600);
}

/// A stated time is used as given when it is ahead; a bare clock time on the
/// current UTC day is today, one already past reads as tomorrow capped at an
/// hour from now; a past date falls back to backoff.
#[test]
fn a_stated_retry_time_becomes_a_concrete_time() {
    // 2026-11-24 10:00 UTC.
    let now = UnixSeconds(1_795_514_400);
    let day = now.0 - now.0 % 86_400;
    let cases = [
        (StatedRetry::On(now.0 + 600), Some(now.0 + 600)),
        (StatedRetry::On(now.0), None),
        (StatedRetry::On(now.0 - 60), None),
        (StatedRetry::Today(12 * 60 + 5), Some(day + (12 * 60 + 5) * 60)),
        (StatedRetry::Today(10 * 60 + 30), Some(now.0 + 1_800)),
        // 09:00 has passed: tomorrow 09:00 is the later reading, capped.
        (StatedRetry::Today(9 * 60), Some(now.0 + 3_600)),
        // 10:00 exactly: the same.
        (StatedRetry::Today(10 * 60), Some(now.0 + 3_600)),
    ];
    for (retry, expected) in cases {
        assert_eq!(
            stated(retry, now),
            expected.map(|at| RetryAt(UnixSeconds(at))),
            "{retry:?}"
        );
    }
    // Just before midnight a clock time just after it is tomorrow, within the cap.
    let late = UnixSeconds(day + 86_400 - 60);
    assert_eq!(
        stated(StatedRetry::Today(10), late),
        Some(RetryAt(UnixSeconds(day + 86_400 + 600)))
    );
}

/// After the retry time one attempt probes the account; the rest wait for it.
/// A successful probe clears the cooldown, a limited one cools it for the
/// next, longer step; an ending that says nothing about the limit changes nothing.
#[test]
fn a_due_cooldown_lets_one_probe_through_and_its_ending_decides() {
    let store = Store::<Test>::parse(EXAMPLE.as_bytes()).unwrap();
    let second = store.get(Test::Codex, &name("second")).unwrap();
    let id = |status: &Status<Test>| status.cooling(Test::Codex, &name("second")).cloned();
    let ended = |status: &Status<Test>, started, ended, at| {
        status.ended(
            Test::Codex,
            &name("second"),
            UnixSeconds(started),
            ended,
            UnixSeconds(at),
        )
    };
    let cooled = ended(&Status::default(), 0, Ended::Limited(None), 1_000);
    let due = UnixSeconds(1_300);
    // Exactly one attempt starts once due; while it runs the others are held.
    assert_eq!(may_start(&second, &cooled, 0, due), Ok(()));
    assert_eq!(may_start(&second, &cooled, 1, due), Err(Hold::Probing));
    // An attempt that began before the retry time is no probe: its success
    // does not clear the cooldown.
    assert_eq!(ended(&cooled, 1_200, Ended::Succeeded, 1_400), cooled);
    assert_eq!(ended(&cooled, 1_300, Ended::Otherwise, 1_400), cooled);
    assert_eq!(ended(&cooled, 1_300, Ended::Succeeded, 1_400), Status::default());
    // A probe that hits the limit again: the next step, from when it ended.
    let second_step = ended(&cooled, 1_300, Ended::Limited(None), 1_310);
    let cooling = id(&second_step).unwrap();
    assert_eq!(
        (cooling.since, cooling.until, cooling.strikes.get()),
        (UnixSeconds(1_000), RetryAt(UnixSeconds(1_310 + 15 * 60)), 2)
    );
    assert_eq!(
        may_start(&second, &second_step, 0, UnixSeconds(1_400)),
        Err(Hold::Cooling(RetryAt(UnixSeconds(2_210))))
    );
    let third = ended(&second_step, 2_210, Ended::Limited(None), 2_220);
    assert_eq!(id(&third).unwrap().until, RetryAt(UnixSeconds(2_220 + 3_600)));
    let fourth = ended(&third, 5_820, Ended::Limited(None), 5_830);
    assert_eq!(
        id(&fourth).unwrap().until,
        RetryAt(UnixSeconds(5_830 + 3_600)),
        "capped"
    );
    // A stated time sets the next retry whatever the step.
    let stated = ended(&fourth, 9_430, Ended::Limited(Some(StatedRetry::On(90_000))), 9_440);
    assert_eq!(id(&stated).unwrap().until, RetryAt(UnixSeconds(90_000)));
    // Success then resets the count: the next limit starts at the first step.
    let cleared = ended(&stated, 90_000, Ended::Succeeded, 90_100);
    let fresh = ended(&cleared, 90_200, Ended::Limited(None), 90_210);
    assert_eq!(id(&fresh).unwrap().strikes, Strikes::FIRST);
}

/// The installed node holds a cooldown saved before retry times existed:
/// `until` absent or null. It loads as due one first step after it began.
#[test]
fn an_old_cooldown_without_a_retry_time_loads_with_the_first_step() {
    for saved in [
        r#"{"cooling":[{"backend":"codex","name":"default","since":1000,"until":null}]}"#,
        r#"{"cooling":[{"backend":"codex","name":"default","since":1000}]}"#,
    ] {
        let status = serde_json::from_str::<Status<Test>>(saved).unwrap();
        let cooling = status.cooling(Test::Codex, &AccountName::implicit()).unwrap();
        assert_eq!(
            (cooling.until, cooling.strikes),
            (RetryAt(UnixSeconds(1_300)), Strikes::FIRST),
            "{saved}"
        );
        let account = default_for(Test::Codex);
        assert_eq!(
            may_start(&account, &status, 0, UnixSeconds(5_000)),
            Ok(()),
            "a probe may start"
        );
    }
    assert!(
        serde_json::from_str::<Status<Test>>(r#"{"cooling":[{"backend":"codex","name":"default","since":1,"x":1}]}"#)
            .is_err()
    );
}

#[test]
fn a_pool_takes_accounts_of_one_kind_only() {
    use AccountKind::{ApiKey, Cloud, Subscription};
    let mixed = |kinds: &[AccountKind]| MixedKinds(kinds.to_vec());
    let table: [(&[AccountKind], Result<(), MixedKinds>); 7] = [
        (&[], Ok(())),
        (&[Subscription], Ok(())),
        (&[ApiKey, ApiKey], Ok(())),
        (&[Subscription, Subscription, Subscription], Ok(())),
        (&[Subscription, ApiKey], Err(mixed(&[Subscription, ApiKey]))),
        (&[ApiKey, Subscription, ApiKey], Err(mixed(&[ApiKey, Subscription]))),
        (
            &[Subscription, Cloud, ApiKey],
            Err(mixed(&[Subscription, Cloud, ApiKey])),
        ),
    ];
    table
        .into_iter()
        .for_each(|(kinds, expected)| assert_eq!(one_kind(kinds.iter().copied()), expected, "{kinds:?}"));
    // In a store: an enabled key beside the implicit subscription refuses the
    // pool and says why; disabled, it is not in the pool.
    let store = |enabled: bool| {
        Store::<Test>::parse(
            format!(r#"{{"accounts":[{{"backend":"codex","name":"key","kind":"api_key","enabled":{enabled}}}]}}"#)
                .as_bytes(),
        )
        .unwrap()
    };
    let refused = store(true)
        .choose(Test::Codex, &AccountChoice::Pool, |_| 0, |_| false)
        .unwrap_err();
    assert_eq!(refused.why, Unusable::Mixed(mixed(&[Subscription, ApiKey])));
    assert!(
        store(false)
            .choose(Test::Codex, &AccountChoice::Pool, |_| 0, |_| false)
            .is_ok()
    );
    // A named account of either kind is still chosen by name.
    assert_eq!(
        store(true)
            .choose(Test::Codex, &name("key").into(), |_| 0, |_| false)
            .unwrap()
            .kind,
        ApiKey
    );
}

#[test]
fn an_api_key_account_keeps_its_key_file_and_runs_uncapped() {
    assert_eq!(AccountKind::try_from("api-key"), Ok(AccountKind::ApiKey));
    assert_eq!(AccountKind::try_from("api_key"), Ok(AccountKind::ApiKey));
    assert_eq!(AccountKind::ApiKey.as_str(), "api-key");
    assert!(AccountKind::try_from("key").is_err());
    let store = Store::<Test>::parse(
        br#"{"accounts":[{"backend":"codex","name":"key","kind":"api_key"},{"backend":"codex","name":"plan","kind":"subscription"}]}"#,
    )
    .unwrap();
    let key = store.get(Test::Codex, &name("key")).unwrap();
    assert_eq!(
        key.secret,
        SecretRef(vec![Location::Config(
            RelativePath::try_from("accounts/codex/key/auth.json").unwrap()
        )])
    );
    assert_eq!(key.cap, None);
    assert_eq!(store.get(Test::Codex, &name("plan")).unwrap().cap, None);
    // The file keeps serde's spelling.
    assert!(
        String::from_utf8(store.encode())
            .unwrap()
            .contains(r#""kind": "api_key""#)
    );
    // A backend that takes no key has no API-key accounts.
    let refused = Store::<Test>::parse(br#"{"accounts":[{"backend":"claude","name":"k","kind":"api_key"}]}"#)
        .unwrap_err()
        .to_string();
    assert!(refused.contains("takes no API key"), "{refused}");
}
