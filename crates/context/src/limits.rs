//! Local chat limits shared by the UI and ingress adapters.
pub const CHAT_MESSAGE_CHARS: usize = 20_000;
pub const CHAT_MESSAGE_ERROR: &str = "Enter 1–20,000 characters.";
// Percent-encoding a four-byte Unicode scalar needs twelve bytes. Leave room
// for form fields and JSON-RPC framing at the maximum message length.
pub const CHAT_REQUEST_BYTES: usize = 256 * 1024;

pub fn message_too_long(text: &str) -> bool {
    text.trim().chars().take(CHAT_MESSAGE_CHARS + 1).count() > CHAT_MESSAGE_CHARS
}

#[cfg(test)]
mod tests;
