
use super::*;
#[test]
fn syntax_keeps_boundaries_arguments_and_unicode_offsets() {
    for definition in DEFINITIONS {
        assert!(parse(definition.text).is_some());
    }
    for text in [
        "/improvements",
        "/opener /open",
        "hello /spawn",
        "/spawned",
        "",
    ] {
        assert!(parse(text).is_none());
    }
    let text = "\u{2003}/open\t/tmp/猫";
    let command = parse(text).unwrap();
    assert_eq!(&text[command.highlight], "/open");
    assert_eq!(command.tail, "\t/tmp/猫");
    assert_eq!(
        tail("/spawn /improve change", Kind::Spawn),
        Some(" /improve change")
    );
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
