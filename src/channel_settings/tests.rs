use super::*;
use std::collections::HashMap;

fn form(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn the_toggle_saves_enabled_and_prefix_for_known_channels_only() {
    let mut saved = None;
    update("POST", &form(&[("channel", "discord"), ("enabled", "yes"), ("prefix", " ? ")]), |name, settings| { saved = Some((name.to_owned(), settings)); Ok(()) }).unwrap();
    let (name, settings) = saved.unwrap();
    assert_eq!((name.as_str(), settings.enabled, settings.prefix.as_str()), ("discord", true, "?"));
    // An unchecked box turns forwarding off.
    let mut saved = None;
    update("POST", &form(&[("channel", "discord"), ("prefix", "!")]), |_, settings| { saved = Some(settings); Ok(()) }).unwrap();
    assert!(!saved.unwrap().enabled);
    let never = |_: &str, _| -> Result<(), &'static str> { panic!("must not save") };
    assert_eq!(update("POST", &form(&[("channel", "telegram")]), never), Err("Unknown channel"));
    assert_eq!(update("GET", &form(&[("channel", "discord")]), never), Err("Use POST"));
    assert_eq!(update("POST", &form(&[]), never), Err("Missing channel"));
    // Prefix validation is the shared settings rule.
    let invalid = channel::save_settings("discord", channel::Settings { prefix: "abc".into(), ..Default::default() });
    assert!(invalid.unwrap_err().contains("prefix"));
}

#[test]
fn the_panel_offers_each_adapter_with_its_prefix_hint() {
    let html = panel();
    assert!(html.contains("action='/tools/channels'") && html.contains("value='discord'"));
    assert!(html.contains("name='enabled'") && html.contains("name='prefix'") && html.contains("help in a DM"));
    // The toggle is about forwarding only; commands do not depend on it.
    assert!(html.contains("Forward operator requests to") && !html.contains("allow steering"));
    assert!(html.contains("forwarding on or off"));
}
