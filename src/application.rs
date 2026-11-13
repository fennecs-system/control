//! Application operations shared by protocol and presentation adapters.
//! Migrate one operation at a time; drivers retain global-state/persistence
//! ownership until their atomic commit boundaries have been extracted.
pub(crate) mod conversations;
pub(crate) mod messages;
pub(crate) mod runs;

/// Captured caller identity, independent of tool catalogue or transport.
pub struct AttemptRef<'a> {
    pub run: &'a str,
    pub generation: &'a str,
    pub nonce: &'a str,
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Application service contract. Adapters encode commands/results; implementations
/// own authorization and effects without exposing their state or executor.
pub trait MessageOps {
    fn message(
        &self,
        attempt: &AttemptRef<'_>,
        call: messages::MessageCall,
    ) -> std::io::Result<messages::MessageResult>;
}

pub(crate) mod room_wakes;
pub(crate) mod room_notifications;
