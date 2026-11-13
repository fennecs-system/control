
use super::*;
#[test]
fn unicode_limit_counts_scalars_not_bytes_or_utf16_units() {
    let text = "🦊".repeat(CHAT_MESSAGE_CHARS);
    assert!(!message_too_long(&format!("\n{text}\n")));
    assert!(message_too_long(&format!("{text}x")));
}
