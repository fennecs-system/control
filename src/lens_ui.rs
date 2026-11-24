//! Browser defaults are preferences; each chat captures its own context.
pub fn normalize(value: &str) -> &str {
    if control_context::valid_id(value) {
        value
    } else {
        "none"
    }
}
pub fn title(value: &str) -> &str {
    match normalize(value) {
        "engineering" => "Engineering",
        "research" => "Research",
        "none" => "No lens",
        other => other,
    }
}
pub fn selected<'a>(library: &control_context::Library, header: &'a str, query: Option<&'a str>) -> &'a str {
    known(library, requested(header, query))
}
/// The lens a request names, in its query or else its cookie. It reads no
/// state, so a route can take it before locking.
pub fn requested<'a>(header: &'a str, query: Option<&'a str>) -> &'a str {
    query.map(normalize).unwrap_or_else(|| {
        header
            .lines()
            .filter_map(|line| line.split_once(':'))
            .filter(|(key, _)| key.eq_ignore_ascii_case("cookie"))
            .flat_map(|(_, value)| value.split(';'))
            .filter_map(|part| part.trim().split_once('='))
            .find(|(name, _)| *name == "control_lens")
            .map(|(_, value)| normalize(value))
            .unwrap_or("none")
    })
}
/// `candidate` when the library has it, else no lens.
pub fn known<'a>(library: &control_context::Library, candidate: &'a str) -> &'a str {
    if library.get(candidate).is_some() {
        candidate
    } else {
        "none"
    }
}
pub fn cookie(value: &str) -> String {
    format!(
        "Set-Cookie: control_lens={}; Path=/; Max-Age=31536000; HttpOnly; SameSite=Strict\r\n",
        normalize(value)
    )
}
pub fn return_to(value: &str) -> &str {
    if value.chars().any(char::is_control) {
        return "/";
    }
    if value == "/"
        || value == "/dms"
        || value == "/chats"
        || value == "/knowledge"
        || value.starts_with("/knowledge?")
        || value.starts_with("/?")
        // A form rendered before the rename may still name `/chat/ID`; its redirect follows.
        || crate::url_names::dm_route(value).is_some_and(crate::chat::valid_id)
    {
        value
    } else {
        "/"
    }
}
#[cfg(test)]
mod tests;
