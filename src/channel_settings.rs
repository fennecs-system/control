//! The web controls for messaging channels: operator forwarding (off by
//! default) and the command prefix, saved to the generic `channels.json`
//! that every channel adapter reads on each poll.
use control::client::channel;
use std::io::Write;

/// Each adapter's forwarding toggle and prefix, for the Connections page.
pub(crate) fn panel() -> String {
    channel::ADAPTERS.iter().map(|(name, title)| {
        let settings = channel::settings(name);
        let prefix = crate::escape(&settings.prefix);
        format!("<form class='decision-form channel-settings' method='post' action='/tools/channels'><input type='hidden' name='channel' value='{name}'><label><input type='checkbox' name='enabled' value='yes'{}> Forward operator requests to {} (architects only)</label><label>Command prefix <input name='prefix' value='{prefix}' size='3' maxlength='3' required pattern='[!-/:-@\\[-`{{-~]{{1,3}}'></label><button>Save</button><p class='muted'>Commands and steering work for architects with forwarding on or off. Type {prefix}help in a DM to see them.</p></form>",
            if settings.enabled { " checked" } else { "" }, crate::escape(title))
    }).collect()
}

/// `POST /tools/channels` from the Connections page.
pub fn http(stream: &mut impl Write, method: &str, path: &str, body: &[u8]) -> bool {
    if path != "/tools/channels" {
        return false;
    }
    let fields = crate::fields(&String::from_utf8_lossy(body));
    let result = update(method, &fields, channel::save_settings);
    let back = "/?view=integrations";
    let _ = match result {
        Ok(()) => control_http::respond(stream, "303 See Other", "text/plain", b"", &format!("Location: {back}\r\n"), control_http::Policy::Page),
        Err(error) => control_http::respond(stream, "400 Bad Request", "text/html; charset=utf-8", crate::error_page(error, back).as_bytes(), "", control_http::Policy::Page),
    };
    true
}
fn update(method: &str, fields: &std::collections::HashMap<String, String>, save: impl FnOnce(&str, channel::Settings) -> Result<(), &'static str>) -> Result<(), &'static str> {
    if method != "POST" {
        return Err("Use POST");
    }
    let name = fields.get("channel").map(String::as_str).ok_or("Missing channel")?;
    if !channel::ADAPTERS.iter().any(|(adapter, _)| *adapter == name) {
        return Err("Unknown channel");
    }
    let mut settings = channel::settings(name);
    settings.enabled = fields.get("enabled").is_some_and(|value| value == "yes");
    settings.prefix = fields.get("prefix").map_or(settings.prefix, |prefix| prefix.trim().to_owned());
    save(name, settings)
}

#[cfg(test)]
mod tests;
