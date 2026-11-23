use super::*;
#[test]
fn syntax_keeps_boundaries_arguments_and_unicode_offsets() {
    for definition in DEFINITIONS {
        assert!(parse(definition.text).is_some());
    }
    for text in ["/improvements", "/opener /open", "hello /spawn", "/spawned", ""] {
        assert!(parse(text).is_none());
    }
    let text = "\u{2003}/open\t/tmp/猫";
    let command = parse(text).unwrap();
    assert_eq!(&text[command.highlight], "/open");
    assert_eq!(command.tail, "\t/tmp/猫");
    assert_eq!(tail("/spawn /improve change", Kind::Spawn), Some(" /improve change"));
    assert!(tail("/spawn /improve change", Kind::Improve).is_none());
    for text in ["/improve off", "/improve on request"] {
        let command = parse(text).unwrap();
        assert_eq!(
            &text[command.highlight],
            if text.ends_with("off") {
                "/improve off"
            } else {
                "/improve on"
            }
        );
    }
    assert_eq!(completions("/op"), vec![7]);
    assert!(completions("/open\n").is_empty());
}

#[test]
fn memory_parses_its_switches_and_compact_is_its_status_alias() {
    let text = "/memory on local-qwen";
    let command = parse(text).unwrap();
    assert_eq!((command.kind, &text[command.highlight]), (Kind::Memory, "/memory on"));
    assert_eq!(command.tail, " on local-qwen");
    assert_eq!(&"/memory off"[parse("/memory off").unwrap().highlight], "/memory off");
    assert_eq!(tail("/memory", Kind::Memory), Some(""));
    assert_eq!(tail("/compact", Kind::Compact), Some(""));
    assert!(parse("/memorize").is_none() && parse("/compaction").is_none());
    let names = completions("/mem")
        .into_iter()
        .map(|i| DEFINITIONS[i].text)
        .collect::<Vec<_>>();
    assert_eq!(names, ["/memory", "/memory on", "/memory off"]);
}

#[test]
fn services_lists_and_highlights_its_stop_subcommand() {
    let text = "/services stop svc-1a2b";
    let command = parse(text).unwrap();
    assert_eq!(
        (command.kind, &text[command.highlight]),
        (Kind::Services, "/services stop")
    );
    assert_eq!(command.tail, " stop svc-1a2b");
    assert_eq!(tail("/services", Kind::Services), Some(""));
    assert_eq!(
        &"/services stopping"[parse("/services stopping").unwrap().highlight],
        "/services"
    );
    assert!(parse("/serviced").is_none());
    let names = completions("/serv")
        .into_iter()
        .map(|i| DEFINITIONS[i].text)
        .collect::<Vec<_>>();
    assert_eq!(names, ["/services", "/services stop"]);
}
