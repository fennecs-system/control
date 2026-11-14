// Shared by the in-container MCP bridge and Control's host mailbox reader, so
// the request limit cannot drift between the two sides of the file mailbox.
/// Largest request envelope, in bytes, that the host reads from a mailbox.
pub const REQUEST_ENVELOPE_BYTES: usize = 32768;
