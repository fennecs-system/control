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
    fn signs_in(self) -> bool {
        true
    }
    fn refreshes(self) -> bool {
        self == Self::Codex
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
        (Unusable::Missing, vec![AccountName::implicit()])
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
    assert_eq!(disabled.why, Unusable::Disabled);
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
        secret(Test::Claude, &name("second")),
        SecretRef(vec![Location::Config(
            RelativePath::try_from("accounts/claude/second/.credentials.json").unwrap()
        )])
    );
    assert_eq!(
        secret(Test::Claude, &AccountName::implicit()),
        Test::Claude.implicit().1
    );
    assert_eq!(
        directory(Test::Codex, &name("second")).as_path(),
        Path::new("accounts/codex/second")
    );
    // A CLI that refreshes its own sign-in runs a named account one attempt at a time.
    let store = Store::<Test>::parse(EXAMPLE.as_bytes()).unwrap();
    let cap = |backend, account: &str| store.get(backend, &name(account)).unwrap().effective_cap();
    assert_eq!(cap(Test::Codex, "second"), NonZeroU8::new(1));
    assert_eq!(cap(Test::Codex, "work"), NonZeroU8::new(2), "an explicit cap wins");
    assert_eq!(cap(Test::Codex, "default"), None, "the implicit account is unchanged");
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
    assert_eq!(may_start(&second, &status, 1, now), Err(Hold::Cap(NonZeroU8::MIN)));
    let cooled = status.cool(Test::Codex, &name("second"), now, None);
    assert_eq!(
        may_start(&second, &cooled, 0, UnixSeconds(9_999)),
        Err(Hold::Cooling(None))
    );
    // A repeated limit keeps when it began; a known retry time lifts it.
    let again = cooled.cool(
        Test::Codex,
        &name("second"),
        UnixSeconds(2_000),
        Some(UnixSeconds(3_000)),
    );
    assert_eq!(
        again
            .cooling(Test::Codex, &name("second"), UnixSeconds(2_500))
            .unwrap()
            .since,
        now
    );
    assert_eq!(may_start(&second, &again, 0, UnixSeconds(3_000)), Ok(()));
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
        r#"{"cooling":[{"backend":"codex","name":"second","since":1000}]}"#
    );
    assert_eq!(serde_json::from_str::<Status<Test>>(&saved).unwrap(), cooled);
}
